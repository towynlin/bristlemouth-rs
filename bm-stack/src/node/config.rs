//! Config messages, `0xA0`–`0xA9`: answering them, and sending them to other
//! nodes.

use bm_wire::BmWireError;
use bm_wire::bcmp::MessageType;
use bm_wire::bcmp::config::{
    ConfigClearRequest, ConfigClearResponse, ConfigCommit, ConfigDeleteRequest,
    ConfigDeleteResponse, ConfigGet, ConfigHeader, ConfigPartitionRequest, ConfigSet,
    ConfigStatusRequest, ConfigValue, MAX_KEY_LEN, MAX_VALUE_LEN, encode_status_response,
    status_response_len,
};
use bm_wire::configuration::{Key, Partition};

use crate::config::Configuration;
use crate::port::{DfuSlot, Identity, NoInitRam, Rtc};
use crate::service::Services;

use super::{Node, Outbound};

#[cfg(doc)]
use super::Event;

impl<'r, I: Identity, R: Rtc, C: Configuration, D: DfuSlot + NoInitRam, S: Services>
    Node<'r, I, R, C, D, S>
{
    /// The `switch` in `bcmp_process_config_message`, for a message addressed
    /// to this node.
    ///
    /// Every answer goes to `FF02::1`, names `source_node_id` as its target
    /// and echoes the request's `seq_num`, which `serialize` keeps because the
    /// four answer types are registered `sequenced_reply`.
    ///
    /// The echoed number is truncated to sixteen bits: every handler in
    /// `bcmp/config.c` takes the `seq_num` as a `uint16_t`, so a request whose
    /// number exceeds `0xFFFF` is answered with only its low half
    /// (divergence #53).
    ///
    /// A partition byte of 3 or more indexes `CONFIGS` out of bounds in every
    /// arm but the clear request's (divergence #50). Here such a message is
    /// answered only where the C checks: a clear request, with `success`
    /// false. The four answer types, when they match no outstanding request,
    /// are only logged by the C, and here produce nothing beyond the
    /// [`Event::Message`] already reported.
    pub(super) fn process_config<'s>(
        &'s mut self,
        now_ms: u32,
        message_type: MessageType,
        source_node_id: u64,
        seq_num: u32,
        payload: &[u8],
    ) -> Option<Outbound<'s>> {
        // `config.c`'s handlers and response builders take a `uint16_t`
        // seq_num, so the echoed number is the request's low sixteen bits.
        let seq_num = u32::from(seq_num as u16);
        let header = ConfigHeader {
            target_node_id: source_node_id,
            source_node_id: self.identity.node_id(),
        };
        match message_type {
            MessageType::CONFIG_GET => {
                let get = ConfigGet::decode(payload).ok()?;
                let partition = Partition::from_u8(get.partition)?;
                let mut slot = [0u8; MAX_VALUE_LEN];
                let len = self
                    .config
                    .store()?
                    .partition(partition)
                    .get_cbor(Key::new(get.key), &mut slot)?;
                let value = ConfigValue {
                    header,
                    partition: get.partition,
                    data: &slot[..len],
                };
                self.send_multicast(
                    now_ms,
                    MessageType::CONFIG_VALUE,
                    seq_num,
                    value.len(),
                    |_, buf| value.encode(buf),
                )
            }
            MessageType::CONFIG_SET => {
                let set = ConfigSet::decode(payload).ok()?;
                if set.data.len() > MAX_VALUE_LEN || set.data.is_empty() {
                    return None;
                }
                let partition = Partition::from_u8(set.partition)?;
                // `set_config_cbor` is handed `keyAndData`, and stores the key
                // with `snprintf("%s")`: it reads on through the value and
                // whatever follows it to the first NUL (divergence #45).
                let key_text = &payload[ConfigSet::HEAD_LEN..];
                let stored = self
                    .config
                    .store_mut()?
                    .partition_mut(partition)
                    .set_cbor(Key::with_len(key_text, set.key.len()), set.data);
                if !stored {
                    return None;
                }
                // The answer carries the value that was sent, not the slot.
                let value = ConfigValue {
                    header,
                    partition: set.partition,
                    data: set.data,
                };
                self.send_multicast(
                    now_ms,
                    MessageType::CONFIG_VALUE,
                    seq_num,
                    value.len(),
                    |_, buf| value.encode(buf),
                )
            }
            MessageType::CONFIG_COMMIT => {
                let commit = ConfigCommit::decode(payload).ok()?;
                let partition = Partition::from_u8(commit.partition)?;
                // `save_config(partition, true)`. Nothing is sent either way.
                self.config.commit(partition);
                None
            }
            MessageType::CONFIG_STATUS_REQUEST => {
                let request = ConfigStatusRequest::decode(payload).ok()?;
                let partition = Partition::from_u8(request.partition)?;
                // Over `bcmp_max_payload_size_bytes` the C sends nothing.
                let len = status_response_len(self.config.store()?.partition(partition))?;
                self.send_multicast(
                    now_ms,
                    MessageType::CONFIG_STATUS_RESPONSE,
                    seq_num,
                    len,
                    |config, buf| {
                        let store = config.store().ok_or(BmWireError::Invalid)?;
                        encode_status_response(
                            buf,
                            header,
                            request.partition,
                            store.partition(partition),
                        )
                    },
                )
            }
            MessageType::CONFIG_DELETE_REQUEST => {
                let request = ConfigDeleteRequest::decode(payload).ok()?;
                let partition = Partition::from_u8(request.partition)?;
                let success = self
                    .config
                    .store_mut()?
                    .partition_mut(partition)
                    .remove_key(Key::new(request.key));
                let response = ConfigDeleteResponse {
                    header,
                    success,
                    partition: request.partition,
                    key: request.key,
                };
                self.send_multicast(
                    now_ms,
                    MessageType::CONFIG_DELETE_RESPONSE,
                    seq_num,
                    response.len(),
                    |_, buf| response.encode(buf),
                )
            }
            MessageType::CONFIG_CLEAR_REQUEST => {
                let request = ConfigClearRequest::decode(payload).ok()?;
                let store = self.config.store_mut()?;
                // `clear_partition` is the one place `config.c`'s partition
                // byte is range-checked, and the answer reports the check.
                let success = match Partition::from_u8(request.partition) {
                    Some(partition) => {
                        store.partition_mut(partition).clear();
                        true
                    }
                    None => false,
                };
                let response = ConfigClearResponse {
                    header,
                    success,
                    partition: request.partition,
                };
                self.send_multicast(
                    now_ms,
                    MessageType::CONFIG_CLEAR_RESPONSE,
                    seq_num,
                    ConfigClearResponse::LEN,
                    |_, buf| response.encode(buf),
                )
            }
            _ => None,
        }
    }

    /// The config store this node answers from, and its storage.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// The same, mutably, for an application that edits its own
    /// configuration.
    pub fn config_mut(&mut self) -> &mut C {
        &mut self.config
    }

    /// Ask `target_node_id` for the value stored under `key` —
    /// `bcmp_config_get`.
    ///
    /// Sequenced: the answer, a [`MessageType::CONFIG_VALUE`] carrying the
    /// whole 50-byte slot, arrives as [`Event::Reply`], and silence as
    /// [`Event::Timeout`]. That is the C with a `reply_cb`; see
    /// [`Node::config_set`] for what differs without one.
    ///
    /// `None` without sending for a key over [`MAX_KEY_LEN`] bytes, as in the
    /// C.
    pub fn config_get(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        partition: Partition,
        key: &[u8],
    ) -> Option<Outbound<'_>> {
        if key.len() > MAX_KEY_LEN {
            return None;
        }
        let get = ConfigGet {
            header: self.config_header(target_node_id),
            partition: partition as u8,
            key,
        };
        self.send_multicast(now_ms, MessageType::CONFIG_GET, 0, get.len(), |_, buf| {
            get.encode(buf)
        })
    }

    /// Store `value`, which should be one CBOR item, under `key` on
    /// `target_node_id` — `bcmp_config_set`.
    ///
    /// Answered with a [`MessageType::CONFIG_VALUE`] echoing `value`, as
    /// [`Event::Reply`]. The target refuses silently a value over
    /// [`MAX_VALUE_LEN`] bytes or one `set_config_cbor` will not classify;
    /// the request then times out.
    ///
    /// Every config request here is the C called **with** a `reply_cb`. A C
    /// caller passing `NULL` instead gets `cfg->process` as its callback
    /// (`serialize` substitutes it), so a matched reply addressed to another
    /// node is re-flooded rather than reported. There is no counterpart here.
    ///
    /// `None` without sending for a key over [`MAX_KEY_LEN`] bytes.
    pub fn config_set(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        partition: Partition,
        key: &[u8],
        value: &[u8],
    ) -> Option<Outbound<'_>> {
        if key.len() > MAX_KEY_LEN {
            return None;
        }
        let set = ConfigSet {
            header: self.config_header(target_node_id),
            partition: partition as u8,
            key,
            data: value,
        };
        self.send_multicast(now_ms, MessageType::CONFIG_SET, 0, set.len(), |_, buf| {
            set.encode(buf)
        })
    }

    /// Ask `target_node_id` to save `partition` and restart —
    /// `bcmp_config_commit`. Unsequenced and unanswered.
    pub fn config_commit(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        partition: Partition,
    ) -> Option<Outbound<'_>> {
        self.config_partition_request(
            now_ms,
            MessageType::CONFIG_COMMIT,
            target_node_id,
            partition,
        )
    }

    /// Ask `target_node_id` which keys `partition` holds —
    /// `bcmp_config_status_request`. The answer is a
    /// [`MessageType::CONFIG_STATUS_RESPONSE`], as [`Event::Reply`];
    /// [`bm_wire::bcmp::config::ConfigStatusResponse::keys`] reads it.
    pub fn config_status_request(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        partition: Partition,
    ) -> Option<Outbound<'_>> {
        self.config_partition_request(
            now_ms,
            MessageType::CONFIG_STATUS_REQUEST,
            target_node_id,
            partition,
        )
    }

    /// Ask `target_node_id` to delete `key` from `partition` —
    /// `bcmp_config_del_key`. Answered with a
    /// [`MessageType::CONFIG_DELETE_RESPONSE`], as [`Event::Reply`].
    ///
    /// The C does not bound the key, and writes its length into a byte: a
    /// 300-byte key goes out with a `key_length` of 44 and all 300 bytes
    /// after it (divergence #51). Here a key over 255 bytes is not sent.
    pub fn config_delete_key(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        partition: Partition,
        key: &[u8],
    ) -> Option<Outbound<'_>> {
        let request = ConfigDeleteRequest {
            header: self.config_header(target_node_id),
            partition: partition as u8,
            key,
        };
        self.send_multicast(
            now_ms,
            MessageType::CONFIG_DELETE_REQUEST,
            0,
            request.len(),
            |_, buf| request.encode(buf),
        )
    }

    /// Ask `target_node_id` to clear `partition` —
    /// `bcmp_config_clear_partition`. Answered with a
    /// [`MessageType::CONFIG_CLEAR_RESPONSE`], as [`Event::Reply`].
    pub fn config_clear_partition(
        &mut self,
        now_ms: u32,
        target_node_id: u64,
        partition: Partition,
    ) -> Option<Outbound<'_>> {
        self.config_partition_request(
            now_ms,
            MessageType::CONFIG_CLEAR_REQUEST,
            target_node_id,
            partition,
        )
    }

    fn config_header(&self, target_node_id: u64) -> ConfigHeader {
        ConfigHeader {
            target_node_id,
            source_node_id: self.identity.node_id(),
        }
    }

    fn config_partition_request(
        &mut self,
        now_ms: u32,
        message_type: MessageType,
        target_node_id: u64,
        partition: Partition,
    ) -> Option<Outbound<'_>> {
        let request = ConfigPartitionRequest {
            header: self.config_header(target_node_id),
            partition: partition as u8,
        };
        self.send_multicast(
            now_ms,
            message_type,
            0,
            ConfigPartitionRequest::LEN,
            |_, buf| request.encode(buf),
        )
    }
}
