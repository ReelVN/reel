//! Sentence timing advances whole runs; visual layout keeps the full text.
use crate::DialogueLine;
use reel_core::text::{TextCueKind, TextSpeed};
use std::time::Duration;

#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct RevealTiming {
    cue: usize,
    speed: TextSpeed,
    fraction: f64,
    pause: f64,
}

impl DialogueLine {
    pub fn playback_complete(&self) -> bool {
        self.revealed_count == self.grapheme_boundaries.len() - 1
            && self.timing.cue == self.content.styled().cues().len()
            && self.timing.pause == 0.
    }

    /// Time until the next change. Hosts freeze this clock while presentation is blocked.
    pub fn playback_interval(&self, player_speed: f64, reveal_skippable: bool) -> Duration {
        if self.playback_complete() {
            return Duration::MAX;
        }
        if player_speed == 0. && reveal_skippable {
            return Duration::ZERO;
        }
        if self.timing.pause > 0. {
            return duration(self.timing.pause);
        }
        if self
            .content
            .styled()
            .cues()
            .get(self.timing.cue)
            .is_some_and(|cue| cue.offset <= self.grapheme_boundaries[self.revealed_count])
        {
            return Duration::ZERO;
        }
        let speed = self.timing.speed.per_second(player_speed);
        if speed == 0. {
            Duration::ZERO
        } else {
            duration((1. - self.timing.fraction) / speed)
        }
    }

    pub(crate) fn skip_timing(&mut self) {
        let visible = self.grapheme_boundaries[self.revealed_count];
        let cues = self.content.styled().cues();
        while let Some(cue) = cues.get(self.timing.cue) {
            if cue.offset > visible {
                break;
            }
            if let TextCueKind::Speed(speed) = cue.kind {
                self.timing.speed = speed;
            }
            self.timing.cue += 1;
        }
        self.timing.pause = 0.;
        self.timing.fraction = 0.;
    }

    pub(crate) fn advance_reveal(
        &mut self,
        elapsed: Duration,
        player_speed: f64,
        reveal_skippable: bool,
    ) -> bool {
        let before = (self.revealed_count, self.playback_complete());
        let total = self.grapheme_boundaries.len() - 1;
        if player_speed == 0. && reveal_skippable {
            self.revealed_count = total;
            self.skip_timing();
        } else {
            let mut seconds = elapsed.as_secs_f64();
            let cues = self.content.styled().cues();
            loop {
                if self.timing.pause > 0. {
                    if seconds < self.timing.pause {
                        self.timing.pause -= seconds;
                        break;
                    }
                    seconds -= self.timing.pause;
                    self.timing.pause = 0.;
                }
                if let Some(cue) = cues.get(self.timing.cue)
                    && cue.offset <= self.grapheme_boundaries[self.revealed_count]
                {
                    self.timing.cue += 1;
                    match cue.kind {
                        TextCueKind::Speed(speed) => self.timing.speed = speed,
                        TextCueKind::Pause(duration) => self.timing.pause = duration.as_secs_f64(),
                    }
                    continue;
                }
                if self.revealed_count == total {
                    break;
                }
                let end = cues.get(self.timing.cue).map_or(total, |cue| {
                    self.grapheme_boundaries
                        .partition_point(|&boundary| boundary < cue.offset)
                });
                let remaining = end - self.revealed_count;
                let speed = self.timing.speed.per_second(player_speed);
                if speed == 0. {
                    self.revealed_count = end;
                } else {
                    let needed = (remaining as f64 - self.timing.fraction) / speed;
                    if seconds >= needed {
                        seconds -= needed;
                        self.revealed_count = end;
                    } else {
                        let progress = self.timing.fraction + seconds * speed;
                        let units = (progress.floor() as usize).min(remaining);
                        self.revealed_count += units;
                        self.timing.fraction = progress - units as f64;
                        break;
                    }
                }
                self.timing.fraction = 0.;
            }
        }
        before != (self.revealed_count, self.playback_complete())
    }
}

fn duration(seconds: f64) -> Duration {
    let value = Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX);
    // Round deadlines upward so a fractional nanosecond cannot busy-wake forever.
    if value.as_secs_f64() < seconds {
        value.saturating_add(Duration::from_nanos(1))
    } else {
        value
    }
}
