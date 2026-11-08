//! PCM sink lead plan and measured stack/output table for physical buffer-feed Loads.
//! The plan is pure; only `resolve_on_load_thread` reads the platform sound output.
use plx_platform::devcaps::AudioStack;

/// How much later than the picture an app-decoded audio unit is stamped for the sink, in
/// nanoseconds, on LG's audio stacks when the sound output is not one that was filmed: 400 ms. The
/// set plays a Load that declares `"audio":"PCM"` through a `PCMMC` mixer channel instead of its
/// `ADEC` decoder (libplayerAPIs `allocateResource`, libpf's resource manager), and LG's renderer
/// applies its DSP lip-sync stage to the hardware decoder and not to a mixer channel. That stage holds
/// the decoder path back by an offset that a PCM Load never is; this lead gives it the same.
///
/// **Where it applies** ([`plx_platform::devcaps::audio_stack`], fingerprinted by the sink plugin; the
/// table is [`pcm_output_lead`]). Everything below is firmware read by hand and by decompile passes
/// (OTA IDs and releases named), never pasted; ONLY the dev set was filmed.
///
/// | Stack | Sets | Effective lip-sync offset | Per-output part |
/// |---|---|---|---|
/// | `liblxa` | 2019-2021 LG chips (dev set W19H 4.10.2), 2022/23 OLEDs before webOS 11 (inferred from the `tools/fwcompat.py` inventories, not from an image read) | 4.5: 505 with a +105 app bias = 400 (MEASURED); W21O 6.5.3: template 400 raw, no +105, effective 400 by the same convention | wireless 150 => Bluetooth 250 (MEASURED) |
/// | `lgaudiosink` | 2022/23 OLEDs on webOS 11 (W22O 11.2.0-74, W23O 11.2.0-3711), 2024 (W24G 9.2.4), 2025 (W25G 10.3.1) | template `{150,150,400,5000,5000}`, 400 raw, no bias | none: 400 on every output |
/// | `audiosink_new` | 2018 (OLED C8 W18O, LCD SK8500 W18H, 4.4.3) | `libkadaptor` default 475 with a +75 app bias = 400 (4.5 inherits it), hand-verified | decoder-only -150 for a wireless output (decompile only) => Bluetooth 250 |
/// | `adecsink` | 2016 (W16M 3.4.3, element `audio_sink`), 2017 (W17H 3.9.2, element `adecsink`) | 2016: default 550, AUTO/mixer rule not in the plugin (not determined). 2017: default 442/472 ms, bias unknown, so differential and DSP bias cannot be split | 2017: decoder only, -100 DD / -150 wireless (a measurement must be per output); 2016: none found |
/// | `other` | Realtek, MStar | their lip sync is NOT decoder-only: no mixer-off rule | none |
///
/// The rule behind the lead is that lip-sync mode AUTO turns the DSP stage ON for the decoder and OFF
/// for a mixer channel, found in `liblxa`'s `lxao_play`, in 2018's `libkadaptor` `Media_Render_Play`
/// (AUTO plus a mixer channel: `EnableLipsync(fd, 0)`) and, with the renderer compiled into the sink and
/// renamed `lgao_*`, in `libgstlgaudiosink.so`. A stack that gets no lead is one where that rule was not
/// found or its constants cannot be separated: Realtek (`rtkalsasink`) and MStar (`omxaudiosink`) have
/// no mixer-off rule (their lip sync is not decoder-only), Realtek Bluetooth is not determined, and
/// MStar may be up to about 128 ms early (its SoC delay floor), which is not "stays right". A set with
/// no lead (`adecsink` on 2016/2017, not determined; Realtek; MStar; an unknown stack) gets NO lead
/// (the behaviour before #1087, 0 ms), because if its PCM path was already in step, 400 ms makes it
/// 0.4 s late. `pcmavoffset` is how such a chip gets measured, and it wins on any stack.
///
/// **`lgaudiosink`: 400 ms on every output, Bluetooth included, no output read.** The offset is the
/// template's 400 raw (webOS 4.5 had 505 with +105; webOS 11 dropped both), and the sink's own 475 ms
/// render delay is applied on both paths and cancels. libpf's `CustomPipeline` never sets the sink's
/// `lipsync` property: CONFIRMED on webOS 11 (in W22O libpf every cross-reference of the string is
/// inside `GenericPipeline::deepElementAddedCallback`). That the per-output table
/// (`LIPSYNC_OFFSET_VALUES`) is unreferenced is checked in part, not proven: on W22O it has no
/// relocation and no `movw`/`movt` pair for its address and its getter `lgao_get_delay` has no direct
/// call site (W24G the same); a pc-relative reference was not ruled out. If it were read after all,
/// Bluetooth would need 250 as on `liblxa` and this gives it 400, about 150 ms late. Per-output
/// adjustment moved to the VIDEO plane (avSync keys by `soundOutput`), which moves both paths equally.
///
/// **The one link NOT proven on webOS 11: the `audio-port` value the `PCMMC` resource reaches the sink
/// with.** The sink's rule is `audio-port == -1` and `mixer-port >= 0` => mixer path. In W22O
/// libplayerAPIs `acquireResources` retypes the ADEC entry to `"PCMMC"` in place and libpf
/// `CustomPipeline::setResource` forwards ports verbatim, so the decision and the sink's rule are the
/// same as on 4.5, where the measurement shows the mixer path; the literal value was not read. If the
/// port were 0 or 1 the sink would stay off the mixer path, AUTO would resolve to ON, app PCM would
/// get the decoder's own 400 ms and be in step, and this lead would make it 0.4 s late. Settled on a
/// set by the sink's own log: `mixer mode changing %s -> %s` and `mixer-port is %d`.
///
/// **`liblxa` on newer firmware** (2021 OLED, 6.5.3): the template is 400 raw with no +105 (effective
/// 400 by the same convention) and its per-output table walk is still there; that the mixer channel is
/// forced to type PCM (0) there was hand-verified on 4.5 only.
///
/// The lead is the mechanism and playback stays straight and lossless: re-encoding to a codec the set
/// decodes itself (which would make the set's own compensation apply) was considered and not taken.
///
/// On `liblxa` and a TV-speaker output that is the whole story. On any other output the set also
/// advances its OWN decoder's audio by a per-output delay that a mixer channel never gets (liblxa's
/// `_lxao_find_lipsync_output_type`, read as reasoning and not copied: wireless 150 ms, Dolby
/// bitstream 160, Atmos 170, DTS 160, WISA 100, ES 30), so the right lead is 400 ms minus the
/// active output's delay. [`PCM_LEAD_BY_OUTPUT`] holds the outputs that were filmed; this is what
/// every other output on `liblxa`, and no answer from the settings service, keeps. 2018's
/// `audiosink_new` is read the same way (all cells not measured); `lgaudiosink` has no such part.
///
/// Filmed ON THE DEV SET ONLY (a `liblxa` set, webOS 4.10.2; flash and beep every 2.5 s, offset = beep minus flash,
/// negative = audio early, FLAC clip): TV speakers, no correction -0.377 s; 330 ms -0.089; 390 ms
/// +0.047 (24 fps camera); 400 ms -0.026 to -0.028 (30 fps), and in a later run -0.03 to +0.08; the
/// set's own decoder on a DTS clip -0.02. Bluetooth headphones (`bt_soundbar`), earcup against the
/// phone's microphone: 400 ms +0.13 to +0.19 s late; 250 ms +0.017 s, three of three; the set's own DTS
/// decoder -0.028. NOT measured: every other stack (`lgaudiosink`, 2018 and 2016/2017 included), any
/// other output (optical, ARC, wired headphones, LG Sound Sync), the user's "AV Sync Adjust" setting
/// (its effect on a mixer channel is not known), and a seek (read from the sink's code, not filmed: it
/// re-derives its render base at every SEGMENT and never from a buffer's pts, so the relative lead
/// survives).
///
/// Applied to the SINK pts of the audio lane's one Feed (`audio_sink_pts` in media), after the speed
/// division, and nowhere else: content-time bookkeeping stays in content time.
pub const PCM_SINK_LEAD_NS: i64 = 400_000_000;

/// The leads that were filmed on `liblxa` (the dev set), by the name the settings service gives the output
/// (`getSystemSettings {"category":"sound","keys":["soundOutput"]}`, `tv::sound`). The TV speakers
/// are listed so that "measured" is a property of the table, not of the default happening to equal
/// them.
pub const PCM_LEAD_BY_OUTPUT: [(&str, i64); 2] = [("tv_speaker", 400_000_000), ("bt_soundbar", 250_000_000)];

/// The lead for a PCM Load on `stack` with the sound output named `output` (`None`: no answer), and
/// whether that cell was measured: THE table, one cell per stack and output.
///
/// | Stack | Lead | Measured |
/// |---|---|---|
/// | [`AudioStack::Liblxa`] | follows the output: [`PCM_LEAD_BY_OUTPUT`] (400 TV speaker, 250 Bluetooth), else 400 | the two listed outputs |
/// | [`AudioStack::AudioSinkNew`] (2018) | the same figures | none |
/// | [`AudioStack::LgAudioSink`] | 400 on every output and for no answer | none |
/// | [`AudioStack::AdecSink`] (2016/2017), `Other`, `Unknown` | 0 whatever the output | none |
///
/// PURE.
pub fn pcm_output_lead(stack: AudioStack, output: Option<&str>) -> (i64, bool) {
    let by_output = || {
        output.and_then(|name| PCM_LEAD_BY_OUTPUT.iter().find(|(known, _)| *known == name)).map(|&(_, ns)| ns)
    };
    match stack {
        AudioStack::Liblxa => by_output().map_or((PCM_SINK_LEAD_NS, false), |ns| (ns, true)),
        AudioStack::AudioSinkNew => (by_output().unwrap_or(PCM_SINK_LEAD_NS), false),
        AudioStack::LgAudioSink => (PCM_SINK_LEAD_NS, false),
        AudioStack::AdecSink | AudioStack::Other | AudioStack::Unknown => (0, false),
    }
}

/// How a Load's audio lead is to be decided, made with the declared audio and the set's audio stack
/// by the media declaration on the frame thread and finished by [`PcmLead::resolve`], where the sound output
/// may be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmLead {
    /// A compressed or absent track: the set's own decoder compensates, so no lead.
    Off,
    /// The `pcmavoffset` dev trigger's value, on the stack it was measured on; it wins over the table
    /// and over the stack.
    Override(i64, AudioStack),
    /// App-decoded audio on a stack with no lead (2016/2017's `adecsink`, Realtek, MStar, or not
    /// known): none was read from the firmware or measured there, so there is none, and the output is
    /// not read.
    NoLead(AudioStack),
    /// App-decoded audio on a stack whose lead is the same on every sound output: the table's answer
    /// for the stack, settled on the frame thread, with no read of the output.
    Fixed(AudioStack),
    /// App-decoded audio on a stack whose lead follows the active sound output (`liblxa`, 2018's
    /// `audiosink_new`), read on the Load thread; the stack is carried for the table and the log line.
    FromOutput(AudioStack),
}

/// What a plan came to: the lead and, for [`PcmLead::FromOutput`], the output it was read from.
pub struct ResolvedLead {
    pub ns: i64,
    plan: PcmLead,
    /// The output's name, or `None` when the plan did not ask or the settings service gave none.
    pub output: Option<String>,
}

impl PcmLead {
    /// What the Engine starts with: the lead the frame thread can settle without reading the output,
    /// and whether the Load thread's read is still owed (then the lead is final only once
    /// `settle_pcm_lead` in media has taken it). PURE.
    pub fn at_engine_start(self) -> (i64, bool) {
        match self {
            Self::Off | Self::NoLead(_) => (0, false),
            Self::Override(ns, _) => (ns, false),
            Self::Fixed(stack) => (pcm_output_lead(stack, None).0, false),
            Self::FromOutput(_) => (0, true),
        }
    }

    /// What the frame thread logs when it starts the Load: the plans it settles itself (the dev
    /// override, a stack with no lead, a stack whose lead is the same on every output). A lead that follows the output is logged by the Load thread
    /// with the output it read, so a Load never writes a second, guessed line first. PURE.
    pub fn start_log_line(self) -> Option<String> {
        match self {
            Self::Override(..) | Self::NoLead(_) | Self::Fixed(_) => self.resolve(|| None).log_line(),
            Self::Off | Self::FromOutput(_) => None,
        }
    }

    /// Settle the plan. `read_output` runs only for [`PcmLead::FromOutput`] (a compressed Load, one
    /// with the dev override and one on a stack with no lead never ask the platform) and is a blocking
    /// read, so only the Load thread may pass one that reaches the bus. PURE apart from that closure.
    pub fn resolve(self, read_output: impl FnOnce() -> Option<String>) -> ResolvedLead {
        match self {
            Self::Off | Self::NoLead(_) => ResolvedLead { ns: 0, plan: self, output: None },
            Self::Override(ns, _) => ResolvedLead { ns, plan: self, output: None },
            Self::Fixed(stack) => ResolvedLead { ns: pcm_output_lead(stack, None).0, plan: self, output: None },
            Self::FromOutput(stack) => {
                let output = read_output();
                let ns = pcm_output_lead(stack, output.as_deref()).0;
                ResolvedLead { ns, plan: self, output }
            }
        }
    }
}

impl ResolvedLead {
    /// The line the Load logs for its lead, once per Load; `None` for a track with no lead.
    pub fn log_line(&self) -> Option<String> {
        let ms = self.ns / 1_000_000;
        match self.plan {
            PcmLead::Off => None,
            PcmLead::Override(_, stack) => {
                Some(format!("pcm: audio sink pts +{ms}ms (pcmavoffset override, stack {})", stack.label()))
            }
            PcmLead::NoLead(stack) => {
                Some(format!("pcm: audio sink pts +{ms}ms (stack {}, not measured on this chip)", stack.label()))
            }
            PcmLead::Fixed(stack) => {
                let note = if pcm_output_lead(stack, None).1 { "" } else { ", not measured" };
                Some(format!("pcm: audio sink pts +{ms}ms (stack {}, every output{note})", stack.label()))
            }
            PcmLead::FromOutput(stack) => {
                let measured = pcm_output_lead(stack, self.output.as_deref()).1;
                let name = self.output.as_deref().unwrap_or("unknown");
                let note = if measured { "" } else { ", not measured" };
                let default_ms = PCM_SINK_LEAD_NS / 1_000_000;
                Some(format!(
                    "pcm: audio sink pts +{ms}ms (stack {}, output {name}, default {default_ms}ms{note})",
                    stack.label()
                ))
            }
        }
    }
}

/// Resolve `plan` on the calling (Load) thread with the real platform read, and log what it decided.
/// The one place the platform is asked for the output.
pub fn resolve_on_load_thread(plan: PcmLead) -> Option<i64> {
    if !matches!(plan, PcmLead::FromOutput(_)) {
        return None;
    }
    let resolved = plan.resolve(plx_platform::tv::sound::output);
    if let Some(line) = resolved.log_line() {
        plx_base::eventlog::log(&line);
    }
    Some(resolved.ns)
}

/// The plan for a Load that declared `audio` on a set whose audio stack is `stack`, given the dev
/// override's text (milliseconds): nothing for anything but `"PCM"` (the override moves only the PCM
/// lead), the override when it is an integer (on any stack: it is how another chip gets measured),
/// otherwise no lead unless the stack follows the sound output (`liblxa`, 2018's `audiosink_new`:
/// [`PcmLead::FromOutput`] with the stack) or is the `lgaudiosink` one, where the lead is the same on
/// every output and needs no read. PURE.
pub fn pcm_lead_plan(audio: &str, override_ms: Option<&str>, stack: AudioStack) -> PcmLead {
    if audio != "PCM" {
        return PcmLead::Off;
    }
    if let Some(ms) = override_ms.and_then(|s| s.trim().parse::<i64>().ok()) {
        return PcmLead::Override(ms.saturating_mul(1_000_000), stack);
    }
    match stack {
        AudioStack::Liblxa | AudioStack::AudioSinkNew => PcmLead::FromOutput(stack),
        AudioStack::LgAudioSink => PcmLead::Fixed(stack),
        AudioStack::AdecSink | AudioStack::Other | AudioStack::Unknown => PcmLead::NoLead(stack),
    }
}
