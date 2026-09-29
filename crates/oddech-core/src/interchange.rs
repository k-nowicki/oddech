//! Text formats for other tools: the recording cut at breaths for editing
//! software (FCPXML) and region labels as training data (CSV, JSON).

use std::fmt::Write as _;

use serde::Serialize;

use crate::region::{Region, RegionKind, RegionSource};

/// A breath marker on the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub start: f64,
    pub end: f64,
    pub name: String,
    pub note: String,
    /// Attenuation of this breath in dB (`-inf` = mute).
    pub gain_db: f32,
}

/// A piece of the cut timeline, in frames: speech between breaths, or a
/// breath (with the marker it came from).
#[derive(Debug, Clone, PartialEq)]
pub struct Segment<'a> {
    pub start: u64,
    pub end: u64,
    pub breath: Option<&'a Marker>,
}

/// Cuts `0..total` frames at every breath edge (snapped to the nearest
/// frame). Overlapping or touching breaths become one segment.
pub fn cut_segments(markers: &[Marker], fps: f64, total: u64) -> Vec<Segment<'_>> {
    let frame = |t: f64| ((t * fps).round().max(0.0) as u64).min(total);
    let mut breaths: Vec<(u64, u64, &Marker)> = markers
        .iter()
        .map(|m| {
            let s = frame(m.start);
            (s, frame(m.end).max(s + 1).min(total), m)
        })
        .filter(|(s, e, _)| e > s)
        .collect();
    breaths.sort_by_key(|b| b.0);
    let mut merged: Vec<(u64, u64, &Marker)> = Vec::new();
    for b in breaths {
        match merged.last_mut() {
            Some(last) if b.0 <= last.1 => last.1 = last.1.max(b.1),
            _ => merged.push(b),
        }
    }
    let mut out = Vec::new();
    let mut pos = 0;
    for (s, e, m) in merged {
        if s > pos {
            out.push(Segment { start: pos, end: s, breath: None });
        }
        out.push(Segment { start: s, end: e, breath: Some(m) });
        pos = e;
    }
    if pos < total {
        out.push(Segment { start: pos, end: total, breath: None });
    }
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Rational seconds for FCPXML, snapped to the frame grid.
struct FrameTime {
    /// Frame duration as `num/den` seconds.
    num: u64,
    den: u64,
}

impl FrameTime {
    fn new(fps: f64) -> Self {
        // Common NTSC rates need exact rationals.
        let table: [(f64, u64, u64); 8] = [
            (23.976, 1001, 24000),
            (29.97, 1001, 30000),
            (59.94, 1001, 60000),
            (24.0, 100, 2400),
            (25.0, 100, 2500),
            (30.0, 100, 3000),
            (50.0, 100, 5000),
            (60.0, 100, 6000),
        ];
        table
            .iter()
            .find(|(f, _, _)| (f - fps).abs() < 0.01)
            .map_or(Self { num: 100, den: (fps.max(1.0) * 100.0).round() as u64 }, |&(_, num, den)| Self { num, den })
    }

    fn frames(&self, secs: f64) -> u64 {
        (secs * self.den as f64 / self.num as f64).round().max(0.0) as u64
    }

    fn fmt(&self, frames: u64) -> String {
        if frames == 0 { "0s".into() } else { format!("{}/{}s", frames * self.num, self.den) }
    }
}

pub struct FcpxmlMedia<'a> {
    pub name: &'a str,
    /// Absolute path of the media file.
    pub path: &'a std::path::Path,
    pub duration: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub audio_channels: u16,
    /// Number of audio streams (tracks) in the file.
    pub audio_sources: usize,
    pub audio_rate: u32,
}

/// FCPXML 1.9 project: the recording cut at every breath edge; each breath
/// clip is attenuated (`adjust-volume`, editable in the editor) and carries
/// a marker. `start_secs` is the timeline's start timecode.
pub fn fcpxml(media: &FcpxmlMedia<'_>, start_secs: u32, markers: &[Marker]) -> String {
    let ft = FrameTime::new(media.fps);
    let total = ft.frames(media.duration).max(1);
    let dur = ft.fmt(total);
    let tc_start = ft.fmt(ft.frames(f64::from(start_secs)));
    let src = file_url(media.path);
    let name = xml_escape(media.name);
    let mut out = String::new();
    let _ = write!(
        out,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.9">
  <resources>
    <format id="r1" name="FFVideoFormat{h}p" frameDuration="{fd}" width="{w}" height="{h}"/>
    <asset id="r2" name="{name}" start="0s" duration="{dur}" hasVideo="1" hasAudio="1" format="r1" audioSources="{sources}" audioChannels="{ch}" audioRate="{rate}">
      <media-rep kind="original-media" src="{src}"/>
    </asset>
  </resources>
  <library>
    <event name="Oddech">
      <project name="{name} – wdechy">
        <sequence format="r1" duration="{dur}" tcStart="{tc_start}" tcFormat="NDF" audioLayout="stereo" audioRate="{rate_k}">
          <spine>
"#,
        fd = ft.fmt(1),
        w = media.width.max(1),
        h = media.height.max(1),
        ch = media.audio_channels.max(1),
        sources = media.audio_sources.max(1),
        rate = media.audio_rate,
        rate_k = if media.audio_rate == 44_100 { "44.1k" } else { "48k" },
    );
    let seconds_per_frame = ft.num as f64 / ft.den as f64;
    for seg in cut_segments(markers, 1.0 / seconds_per_frame, total) {
        let (start, len) = (ft.fmt(seg.start), ft.fmt(seg.end - seg.start));
        let offset = ft.fmt(seg.start + ft.frames(f64::from(start_secs)));
        let _ = write!(
            out,
            r#"            <asset-clip ref="r2" offset="{offset}" name="{name}" start="{start}" duration="{len}" format="r1" tcFormat="NDF""#
        );
        match seg.breath {
            None => out.push_str("/>\n"),
            Some(m) => {
                // FCPXML cannot express -inf; -96 dB is silence in practice.
                let db = if m.gain_db.is_finite() { m.gain_db.max(-96.0) } else { -96.0 };
                let _ = writeln!(out, ">");
                let _ = writeln!(out, r#"              <adjust-volume amount="{db:.1}dB"/>"#);
                let _ = writeln!(
                    out,
                    r#"              <marker start="{start}" duration="{len}" value="{}" note="{}"/>"#,
                    xml_escape(&m.name),
                    xml_escape(&m.note)
                );
                out.push_str("            </asset-clip>\n");
            }
        }
    }
    out.push_str("          </spine>\n        </sequence>\n      </project>\n    </event>\n  </library>\n</fcpxml>\n");
    out
}

fn file_url(path: &std::path::Path) -> String {
    let mut url = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => url.push(b as char),
            _ => {
                let _ = write!(url, "%{b:02X}");
            }
        }
    }
    url
}

/// One labelled region as exported for training.
#[derive(Debug, Clone, Serialize)]
pub struct Label<'a> {
    pub track: u32,
    pub track_name: &'a str,
    pub start: f64,
    pub end: f64,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

impl<'a> Label<'a> {
    pub fn from_region(r: &Region, track_name: &'a str) -> Self {
        Self {
            track: r.track.0,
            track_name,
            start: r.start,
            end: r.end,
            kind: match r.kind {
                RegionKind::Breath => "breath",
                RegionKind::NotBreath => "not_breath",
            },
            source: match r.source {
                RegionSource::Model => "model",
                RegionSource::User => "user",
            },
            confidence: r.confidence,
        }
    }
}

pub fn labels_csv(labels: &[Label<'_>]) -> String {
    let mut out = String::from("track,track_name,start,end,type,source,confidence\n");
    for l in labels {
        let name = l.track_name.replace('"', "\"\"");
        let conf = l.confidence.map(|c| format!("{c:.4}")).unwrap_or_default();
        let _ = writeln!(out, "{},\"{name}\",{:.6},{:.6},{},{},{conf}", l.track, l.start, l.end, l.kind, l.source);
    }
    out
}

pub fn labels_json(media_file: &str, labels: &[Label<'_>]) -> String {
    #[derive(Serialize)]
    struct Doc<'a> {
        media_file: &'a str,
        labels: &'a [Label<'a>],
    }
    serde_json::to_string_pretty(&Doc { media_file, labels }).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::RegionId;
    use crate::track::TrackId;

    fn marker(start: f64, end: f64) -> Marker {
        Marker { start, end, name: "Wdech 1".into(), note: "Rode".into(), gain_db: -9.0 }
    }

    #[test]
    fn segments_cover_everything_and_merge_overlaps() {
        let ms = [marker(1.0, 1.5), marker(1.4, 2.0), marker(5.0, 5.01)];
        let s = cut_segments(&ms, 30.0, 300);
        let spans: Vec<(u64, u64, bool)> = s.iter().map(|g| (g.start, g.end, g.breath.is_some())).collect();
        // 5.01 s is less than a frame long: it still gets one frame.
        assert_eq!(spans, vec![(0, 30, false), (30, 60, true), (60, 150, false), (150, 151, true), (151, 300, false)]);
    }

    #[test]
    fn fcpxml_is_frame_aligned_and_escaped() {
        let path = std::path::Path::new("/tmp/a b&c.mkv");
        let media = FcpxmlMedia {
            name: "a b&c",
            path,
            duration: 10.0,
            fps: 29.97,
            width: 1920,
            height: 1080,
            audio_channels: 2,
            audio_sources: 3,
            audio_rate: 48_000,
        };
        let x = fcpxml(&media, 0, &[marker(1.0, 1.5)]);
        assert!(x.contains(r#"frameDuration="1001/30000s""#));
        assert!(x.contains("file:///tmp/a%20b%26c.mkv"));
        assert!(x.contains(r#"name="a b&amp;c""#));
        // 1.0 s at 29.97 → frame 30 → 30030/30000 s.
        assert!(x.contains(r#"<marker start="30030/30000s""#), "{x}");
        assert!(x.contains(r#"audioSources="3""#));
        // Three clips: before, the breath (attenuated), after.
        assert_eq!(x.matches("<asset-clip ").count(), 3, "{x}");
        assert!(x.contains(r#"<adjust-volume amount="-9.0dB"/>"#));
        assert!(x.contains(r#"offset="30030/30000s" name="a b&amp;c" start="30030/30000s" duration="15015/30000s""#), "{x}");
    }

    #[test]
    fn labels_round_trip_fields() {
        let r = Region {
            id: RegionId(1),
            track: TrackId(3),
            start: 1.25,
            end: 1.5,
            kind: RegionKind::NotBreath,
            source: RegionSource::User,
            confidence: Some(0.7),
            gain_db: None,
            fade_ms: None,
            silence_db: Vec::new(),
        };
        let labels = [Label::from_region(&r, "Mikrofon \"A\"")];
        let csv = labels_csv(&labels);
        assert!(csv.lines().nth(1).unwrap().starts_with("3,\"Mikrofon \"\"A\"\"\",1.250000,1.500000,not_breath,user,0.7000"));
        let json: serde_json::Value = serde_json::from_str(&labels_json("x.mkv", &labels)).unwrap();
        assert_eq!(json["labels"][0]["type"], "not_breath");
    }
}
