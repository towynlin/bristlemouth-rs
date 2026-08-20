// Deterministic, single-threaded implementation of bm_core's bm_os.h.
//
// Nothing here sleeps, spawns a thread, or reads a wall clock. See
// csrc/bm_shim.h and README.md for the contract this upholds.

#include "bm_shim.h"

#include "bm_config.h"
#include "bm_os.h"

#include <setjmp.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_TASKS 16
#define MAX_TIMERS 32
#define DEFAULT_PUMP_BUDGET 256

typedef struct {
  uint32_t item_size;
  uint32_t capacity;
  uint32_t count;
  uint32_t head;
  uint8_t *storage;
} ShimQueue;

typedef struct {
  uint32_t capacity;
  uint32_t count;
  uint32_t head;
  uint8_t *storage;
} ShimStream;

typedef struct {
  int32_t count;
} ShimSemaphore;

typedef struct {
  uint32_t period_ms;
  bool auto_reload;
  bool active;
  uint32_t due_tick;
  void *timer_id;
  BmTimerCallback cb;
} ShimTimer;

typedef struct {
  BmTask func;
  void *arg;
} ShimTask;

static struct {
  uint32_t tick;
  bool debug_on;

  ShimTask tasks[MAX_TASKS];
  uint32_t task_count;

  // Non-NULL only while bm_shim_pump is inside a task body.
  jmp_buf *escape;
  uint32_t budget;
  uint32_t budget_limit;

  ShimTimer *timers[MAX_TIMERS];
  uint32_t timer_count;
} CTX = {.budget_limit = DEFAULT_PUMP_BUDGET};

// A task that can make no further progress leaves its `while (true)` loop the
// only way C allows: by unwinding to the pump. bm_core's loops call the
// blocking primitive as the first statement of the iteration, so nothing is
// live across the jump.
static void yield_to_pump(void) {
  if (CTX.escape) {
    jmp_buf *target = CTX.escape;
    CTX.escape = NULL;
    longjmp(*target, 1);
  }
}

// Called at every blocking primitive. Returns true if the caller should give
// up its slice, either because it is idle or because it has burned its budget.
static bool should_yield(bool resource_available) {
  if (!CTX.escape) {
    return false; // not inside a pump; just report the timeout
  }
  if (!resource_available) {
    return true;
  }
  if (CTX.budget == 0) {
    return true;
  }
  CTX.budget--;
  return false;
}

// ---------------------------------------------------------------------------
// Debug output
// ---------------------------------------------------------------------------

void bm_shim_set_debug(bool on) { CTX.debug_on = on; }

void bm_shim_debug_printf(const char *format, ...) {
  if (!CTX.debug_on) {
    return;
  }
  va_list args;
  va_start(args, format);
  vfprintf(stderr, format, args);
  va_end(args);
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

void *bm_malloc(size_t size) { return malloc(size); }
void bm_free(void *ptr) { free(ptr); }

// ---------------------------------------------------------------------------
// Queues — real bounded FIFOs, so bm_core's message ordering is preserved
// ---------------------------------------------------------------------------

BmQueue bm_queue_create(uint32_t queue_length, uint32_t item_size) {
  if (queue_length == 0 || item_size == 0) {
    return NULL;
  }
  ShimQueue *q = (ShimQueue *)calloc(1, sizeof(*q));
  if (!q) {
    return NULL;
  }
  q->storage = (uint8_t *)calloc(queue_length, item_size);
  if (!q->storage) {
    free(q);
    return NULL;
  }
  q->item_size = item_size;
  q->capacity = queue_length;
  return q;
}

void bm_queue_delete(BmQueue queue) {
  ShimQueue *q = (ShimQueue *)queue;
  if (!q) {
    return;
  }
  free(q->storage);
  free(q);
}

BmErr bm_queue_receive(BmQueue queue, void *item, uint32_t timeout_ms) {
  (void)timeout_ms; // the shim never sleeps
  ShimQueue *q = (ShimQueue *)queue;
  if (!q || !item) {
    return BmEINVAL;
  }
  if (should_yield(q->count > 0)) {
    yield_to_pump();
    return BmETIMEDOUT;
  }
  if (q->count == 0) {
    return BmETIMEDOUT;
  }
  memcpy(item, q->storage + (size_t)q->head * q->item_size, q->item_size);
  q->head = (q->head + 1) % q->capacity;
  q->count--;
  return BmOK;
}

BmErr bm_queue_send(BmQueue queue, const void *item, uint32_t timeout_ms) {
  (void)timeout_ms;
  ShimQueue *q = (ShimQueue *)queue;
  if (!q || !item) {
    return BmEINVAL;
  }
  if (q->count == q->capacity) {
    return BmENOMEM; // full; the shim will not block waiting for space
  }
  uint32_t tail = (q->head + q->count) % q->capacity;
  memcpy(q->storage + (size_t)tail * q->item_size, item, q->item_size);
  q->count++;
  return BmOK;
}

BmErr bm_queue_send_to_front_from_isr(BmQueue queue, const void *item) {
  ShimQueue *q = (ShimQueue *)queue;
  if (!q || !item) {
    return BmEINVAL;
  }
  if (q->count == q->capacity) {
    return BmENOMEM;
  }
  q->head = (q->head + q->capacity - 1) % q->capacity;
  memcpy(q->storage + (size_t)q->head * q->item_size, item, q->item_size);
  q->count++;
  return BmOK;
}

// ---------------------------------------------------------------------------
// Stream buffers
// ---------------------------------------------------------------------------

BmBuffer bm_stream_buffer_create(uint32_t max_size) {
  if (max_size == 0) {
    return NULL;
  }
  ShimStream *s = (ShimStream *)calloc(1, sizeof(*s));
  if (!s) {
    return NULL;
  }
  s->storage = (uint8_t *)calloc(max_size, 1);
  if (!s->storage) {
    free(s);
    return NULL;
  }
  s->capacity = max_size;
  return s;
}

void bm_stream_buffer_delete(BmBuffer buf) {
  ShimStream *s = (ShimStream *)buf;
  if (!s) {
    return;
  }
  free(s->storage);
  free(s);
}

BmErr bm_stream_buffer_send(BmBuffer buf, uint8_t *data, uint32_t size,
                            uint32_t timeout_ms) {
  (void)timeout_ms;
  ShimStream *s = (ShimStream *)buf;
  if (!s || !data) {
    return BmEINVAL;
  }
  if (s->count + size > s->capacity) {
    return BmENOMEM;
  }
  for (uint32_t i = 0; i < size; i++) {
    s->storage[(s->head + s->count + i) % s->capacity] = data[i];
  }
  s->count += size;
  return BmOK;
}

BmErr bm_stream_buffer_receive(BmBuffer buf, uint8_t *data, uint32_t *size,
                               uint32_t timeout_ms) {
  (void)timeout_ms;
  ShimStream *s = (ShimStream *)buf;
  if (!s || !data || !size) {
    return BmEINVAL;
  }
  if (should_yield(s->count > 0)) {
    yield_to_pump();
    return BmETIMEDOUT;
  }
  if (s->count == 0) {
    *size = 0;
    return BmETIMEDOUT;
  }
  uint32_t n = s->count < *size ? s->count : *size;
  for (uint32_t i = 0; i < n; i++) {
    data[i] = s->storage[(s->head + i) % s->capacity];
  }
  s->head = (s->head + n) % s->capacity;
  s->count -= n;
  *size = n;
  return BmOK;
}

// ---------------------------------------------------------------------------
// Mutexes and semaphores — counters, since nothing here is preemptible
// ---------------------------------------------------------------------------

BmSemaphore bm_mutex_create(void) {
  ShimSemaphore *s = (ShimSemaphore *)calloc(1, sizeof(*s));
  if (s) {
    s->count = 1; // a mutex starts unlocked
  }
  return s;
}

BmSemaphore bm_semaphore_create(void) {
  ShimSemaphore *s = (ShimSemaphore *)calloc(1, sizeof(*s));
  return s; // a semaphore starts unsignalled
}

void bm_semaphore_delete(BmSemaphore semaphore) { free(semaphore); }

BmErr bm_semaphore_give(BmSemaphore semaphore) {
  ShimSemaphore *s = (ShimSemaphore *)semaphore;
  if (!s) {
    return BmEINVAL;
  }
  s->count++;
  return BmOK;
}

BmErr bm_semaphore_take(BmSemaphore semaphore, uint32_t timeout_ms) {
  (void)timeout_ms;
  ShimSemaphore *s = (ShimSemaphore *)semaphore;
  if (!s) {
    return BmEINVAL;
  }
  if (should_yield(s->count > 0)) {
    yield_to_pump();
    return BmETIMEDOUT;
  }
  if (s->count <= 0) {
    return BmETIMEDOUT;
  }
  s->count--;
  return BmOK;
}

// ---------------------------------------------------------------------------
// Tasks — recorded, never spawned. bm_shim_pump runs them.
// ---------------------------------------------------------------------------

BmErr bm_task_create(BmTask task, const char *name, uint32_t stack_size,
                     void *arg, uint32_t priority, BmTaskHandle task_handle) {
  (void)name;
  (void)stack_size;
  (void)priority;
  if (!task) {
    return BmEINVAL;
  }
  if (CTX.task_count >= MAX_TASKS) {
    return BmENOMEM;
  }
  ShimTask *slot = &CTX.tasks[CTX.task_count++];
  slot->func = task;
  slot->arg = arg;
  // bm_posix.c documents task_handle as "actually a void **".
  if (task_handle) {
    *(void **)task_handle = (void *)slot;
  }
  return BmOK;
}

void bm_task_delete(BmTaskHandle task_handle) {
  ShimTask *target = (ShimTask *)task_handle;
  if (!target) {
    return;
  }
  for (uint32_t i = 0; i < CTX.task_count; i++) {
    if (&CTX.tasks[i] == target) {
      // Compacting would invalidate handles bm_core already holds, so blank
      // the slot instead; bm_shim_pump skips entries with no function.
      CTX.tasks[i].func = NULL;
      CTX.tasks[i].arg = NULL;
      return;
    }
  }
}

void bm_start_scheduler(void) { /* Rust drives execution via bm_shim_pump */ }

uint32_t bm_shim_task_count(void) { return CTX.task_count; }

void bm_shim_set_pump_budget(uint32_t calls) { CTX.budget_limit = calls; }

uint32_t bm_shim_pump(void) {
  uint32_t ran = 0;
  for (uint32_t i = 0; i < CTX.task_count; i++) {
    ShimTask task = CTX.tasks[i];
    if (!task.func) {
      continue;
    }
    jmp_buf escape;
    if (setjmp(escape) == 0) {
      CTX.escape = &escape;
      CTX.budget = CTX.budget_limit;
      task.func(task.arg);
      // A task body that returns on its own is fine; it just has no loop.
    }
    // Reached either by falling out of the body or by yield_to_pump.
    CTX.escape = NULL;
    ran++;
  }
  return ran;
}

// ---------------------------------------------------------------------------
// Timers — armed against the virtual clock, fired by bm_shim_advance_ticks
// ---------------------------------------------------------------------------

BmTimer bm_timer_create(const char *name, uint32_t period_ms, bool auto_reload,
                        void *timer_id, BmTimerCallback cb) {
  (void)name;
  if (CTX.timer_count >= MAX_TIMERS) {
    return NULL;
  }
  ShimTimer *t = (ShimTimer *)calloc(1, sizeof(*t));
  if (!t) {
    return NULL;
  }
  t->period_ms = period_ms;
  t->auto_reload = auto_reload;
  t->timer_id = timer_id;
  t->cb = cb;
  CTX.timers[CTX.timer_count++] = t;
  return t;
}

void bm_timer_delete(BmTimer timer, uint32_t timeout_ms) {
  (void)timeout_ms;
  ShimTimer *t = (ShimTimer *)timer;
  if (!t) {
    return;
  }
  for (uint32_t i = 0; i < CTX.timer_count; i++) {
    if (CTX.timers[i] == t) {
      CTX.timers[i] = CTX.timers[--CTX.timer_count];
      break;
    }
  }
  free(t);
}

BmErr bm_timer_start(BmTimer timer, uint32_t timeout_ms) {
  (void)timeout_ms;
  ShimTimer *t = (ShimTimer *)timer;
  if (!t) {
    return BmEINVAL;
  }
  t->active = true;
  t->due_tick = CTX.tick + t->period_ms;
  return BmOK;
}

BmErr bm_timer_stop(BmTimer timer, uint32_t timeout_ms) {
  (void)timeout_ms;
  ShimTimer *t = (ShimTimer *)timer;
  if (!t) {
    return BmEINVAL;
  }
  t->active = false;
  return BmOK;
}

BmErr bm_timer_reset(BmTimer timer, uint32_t timeout_ms) {
  return bm_timer_start(timer, timeout_ms);
}

BmErr bm_timer_change_period(BmTimer timer, uint32_t period_ms,
                             uint32_t timeout_ms) {
  (void)timeout_ms;
  ShimTimer *t = (ShimTimer *)timer;
  if (!t) {
    return BmEINVAL;
  }
  t->period_ms = period_ms;
  // Matches FreeRTOS: changing the period restarts the timer.
  t->active = true;
  t->due_tick = CTX.tick + period_ms;
  return BmOK;
}

BmErr bm_timer_is_timer_active(BmTimer timer) {
  ShimTimer *t = (ShimTimer *)timer;
  return (t && t->active) ? BmOK : BmEINVAL;
}

uint32_t bm_timer_get_id(BmTimer timer) {
  ShimTimer *t = (ShimTimer *)timer;
  return t ? (uint32_t)(uintptr_t)t->timer_id : 0;
}

// ---------------------------------------------------------------------------
// Virtual clock — 1 tick == 1 ms, matching bm_posix.c
// ---------------------------------------------------------------------------

uint32_t bm_get_tick_count(void) { return CTX.tick; }
uint32_t bm_get_tick_count_from_isr(void) { return CTX.tick; }
uint32_t bm_ms_to_ticks(uint32_t ms) { return ms; }
uint32_t bm_ticks_to_ms(uint32_t ticks) { return ticks; }
uint32_t bm_shim_tick_count(void) { return CTX.tick; }

// Fire every timer due at or before the current tick, reloading as it goes.
static void fire_due_timers(void) {
  bool fired;
  do {
    fired = false;
    for (uint32_t i = 0; i < CTX.timer_count; i++) {
      ShimTimer *t = CTX.timers[i];
      if (!t->active || (int32_t)(CTX.tick - t->due_tick) < 0) {
        continue;
      }
      if (t->auto_reload) {
        // Guard against a zero period turning this into an infinite loop.
        t->due_tick += t->period_ms ? t->period_ms : 1;
      } else {
        t->active = false;
      }
      if (t->cb) {
        t->cb(t);
      }
      fired = true;
    }
  } while (fired);
}

void bm_shim_advance_ticks(uint32_t ticks) {
  CTX.tick += ticks;
  fire_due_timers();
}

void bm_delay(uint32_t ms) { bm_shim_advance_ticks(ms); }

// ---------------------------------------------------------------------------
// Reset
// ---------------------------------------------------------------------------

void bm_shim_reset(void) {
  bm_shim_generic_reset();
  for (uint32_t i = 0; i < CTX.timer_count; i++) {
    free(CTX.timers[i]);
  }
  uint32_t budget_limit = CTX.budget_limit;
  bool debug_on = CTX.debug_on;
  memset(&CTX, 0, sizeof(CTX));
  CTX.budget_limit = budget_limit;
  CTX.debug_on = debug_on;
}
