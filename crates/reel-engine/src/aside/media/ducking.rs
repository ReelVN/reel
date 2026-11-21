use super::{
    AdapterError, Heap, Live, Media, MediaId, Slot, Value, ValueTag, duration, is_media, volume,
};
use crate::aside::snapshot::{Part, Record};
use aside_vm::{HeapError, ObjectKind, RootToken};
use std::time::Duration;

#[derive(Clone, Copy)]
pub(super) struct Duck {
    voice: MediaId,
    root: RootToken,
    target: MediaId,
    gain: f64,
    release: Duration,
}

impl Duck {
    pub(super) fn snapshot_record(&self) -> Result<Record, AdapterError> {
        Record::new(Part::AudioDuck, &self.root)
    }
}

fn active_audio(
    live: &[Live],
    heap: &Heap,
    value: Value,
) -> Result<Option<(MediaId, RootToken)>, AdapterError> {
    if !is_media(heap, value) {
        return Err(HeapError::TypeMismatch.into());
    }
    let id = heap
        .host_payload(value, Slot::Id as u32)?
        .as_int()
        .map(|id| MediaId(id as u64));
    Ok(id
        .and_then(|id| {
            live.iter()
                .find(|entry| entry.id == id && entry.result.is_none() && entry.video.is_none())
        })
        .map(|entry| (entry.id, entry.root)))
}

pub(super) fn validate_link(heap: &Heap, voice: Value, link: Value) -> Result<(), AdapterError> {
    read_link(heap, voice, link).map(|_| ())
}

fn read_link(
    heap: &Heap,
    voice: Value,
    link: Value,
) -> Result<(Value, f64, Duration), AdapterError> {
    if heap.info(link)?.kind != ObjectKind::Tuple || heap.sequence_len(link)? != 3 {
        return Err(HeapError::TypeMismatch.into());
    }
    let target = heap.sequence_get(link, 0)?;
    let gain = volume(heap.sequence_get(link, 1)?)?;
    if !is_media(heap, target) || target.same_bits(voice) || gain > 1. {
        return Err(AdapterError::Input(
            "audio duck requires distinct playbacks and gain in 0..1",
        ));
    }
    Ok((target, gain, duration(heap.sequence_get(link, 2)?)?))
}

impl Media {
    pub(super) fn apply_duck(
        &mut self,
        heap: &Heap,
        voice: Value,
        link: Value,
        attack: Duration,
    ) -> Result<bool, AdapterError> {
        let (target, gain, release) = read_link(heap, voice, link)?;
        let Some((voice_id, root)) = active_audio(&self.live, heap, voice)? else {
            return Ok(false);
        };
        let Some((target, _)) = active_audio(&self.live, heap, target)? else {
            return Ok(false);
        };
        let next = Duck {
            voice: voice_id,
            root,
            target,
            gain,
            release,
        };
        let current = self.ducks.iter().position(|layer| layer.voice == voice_id);
        if current.is_none() {
            super::super::results::reserve_queue(heap, &mut self.ducks, &mut self.duck_charge)?;
        }
        self.target_duck(target, Some(voice_id), gain, attack)?;
        if let Some(index) = current {
            let old = std::mem::replace(&mut self.ducks[index], next);
            if old.target != target && self.duck_target_live(old.target) {
                self.target_duck(old.target, None, 1., old.release)?;
            }
        } else {
            self.ducks.push_back(next);
        }
        Ok(true)
    }

    pub(super) fn retire_ducks(&mut self, heap: &mut Heap) -> Result<(), AdapterError> {
        let mut index = 0;
        while let Some(layer) = self.ducks.get(index).copied() {
            let target_live = self.duck_target_live(layer.target);
            if target_live && self.duck_target_live(layer.voice) {
                index += 1;
                continue;
            }
            self.ducks.remove(index);
            let voice = heap.root_value(layer.root)?;
            heap.set_host_payload(voice, Slot::Duck as u32, Value::NULL)?;
            if target_live {
                self.target_duck(layer.target, None, 1., layer.release)?;
            }
        }
        Ok(())
    }

    pub(super) fn restore_duck(
        &mut self,
        heap: &mut Heap,
        voice: Value,
    ) -> Result<(), AdapterError> {
        let link = heap.host_payload(voice, Slot::Duck as u32)?;
        if link.tag() != ValueTag::Null && !self.apply_duck(heap, voice, link, Duration::ZERO)? {
            heap.set_host_payload(voice, Slot::Duck as u32, Value::NULL)?;
        }
        Ok(())
    }

    fn duck_target_live(&self, id: MediaId) -> bool {
        self.live
            .iter()
            .any(|live| live.id == id && live.result.is_none())
    }

    fn target_duck(
        &mut self,
        target: MediaId,
        except: Option<MediaId>,
        gain: f64,
        duration: Duration,
    ) -> Result<(), AdapterError> {
        let volume = self
            .ducks
            .iter()
            .filter(|layer| layer.target == target && Some(layer.voice) != except)
            .fold(gain, |gain, layer| gain * layer.gain);
        self.backend
            .as_mut()
            .expect("live duck target has a media backend")
            .duck_audio(target, volume, duration)
            .map_err(AdapterError::Asset)
    }
}
