//! Software audio decode and pitch-corrected speed output.
use crate::ffi::*;
use std::os::raw::{c_int, c_void};

/// **The audio decoder a playback-speed session runs** (`crate::speed_audio`): the selected audio
/// stream decoded in software, downmixed to stereo and time-stretched to `permille`, then fed as
/// PCM AUs. Exists only while the speed is not 1x; at 1x the compressed audio goes to the TV as it
/// always has, except the app-decoded set (FLAC, ALAC, LPCM, and the lossy codecs an attempt decodes in
/// the app, `Caps::app_decodes_lossy_track`), which `ff_pcm::PcmDecoder` decodes and feeds as PCM. At a speed this decoder takes those too and `PcmDecoder` is not
/// opened. Opened per demux run from the stream's own codec parameters.
pub struct SpeedDecoder {
    ctx: *mut AVCodecContext,
    frame: *mut c_void,
    audio: crate::speed_audio::SpeedAudio,
    logged: bool,
}

impl SpeedDecoder {
    /// `None` when the bundled FFmpeg has no decoder for the stream (`ci/build-ffmpeg.sh`'s list)
    /// or it fails to open; the caller then cannot honour the speed.
    pub unsafe fn open(cp: *const AVCodecParameters, permille: i32) -> Option<SpeedDecoder> {
        let codec = avcodec_find_decoder((*cp).codec_id);
        if codec.is_null() {
            return None;
        }
        let ctx = avcodec_alloc_context3(codec);
        if ctx.is_null() {
            return None;
        }
        let frame = av_frame_alloc();
        if frame.is_null()
            || avcodec_parameters_to_context(ctx, cp) < 0
            || avcodec_open2(ctx, codec, std::ptr::null_mut()) < 0
        {
            let mut c = ctx;
            avcodec_free_context(&mut c);
            if !frame.is_null() {
                let mut f = frame;
                av_frame_free(&mut f);
            }
            return None;
        }
        Some(SpeedDecoder { ctx, frame, audio: crate::speed_audio::SpeedAudio::new(permille), logged: false })
    }

    /// Decode one packet whose content time is `pts_ns`; returns the PCM AUs now complete, each
    /// with its CONTENT pts. A packet the decoder rejects yields nothing (the stretch bridges it).
    pub unsafe fn decode(&mut self, pkt: *const AVPacket, pts_ns: i64) -> Vec<(i64, Vec<u8>)> {
        let mut aus = Vec::new();
        if avcodec_send_packet(self.ctx, pkt) < 0 {
            return aus;
        }
        let f = self.frame as *const u8;
        loop {
            // EAGAIN (wants the next packet), EOF and errors all end this packet's frames
            if avcodec_receive_frame(self.ctx, self.frame) < 0 {
                break;
            }
            let rd = |off: usize| std::ptr::read_unaligned(f.add(off) as *const c_int);
            let nb = rd(OFF_AFRAME_NB_SAMPLES).max(0) as usize;
            let rate = rd(OFF_AFRAME_SAMPLE_RATE).max(0) as u32;
            let channels = rd(OFF_AFRAME_NB_CHANNELS).max(0) as usize;
            let fmt = rd(OFF_FRAME_FORMAT); // video's field, read here as an `AVSampleFormat`
            let ext = std::ptr::read_unaligned(f.add(OFF_AFRAME_EXTENDED_DATA) as *const *const *const u8);
            if !self.logged {
                self.logged = true;
                plx_base::eventlog::log(&format!(
                    "speed: decoding audio fmt={fmt} ch={channels} rate={rate} nb={nb}"
                ));
            }
            if let Some((sf, planar)) = crate::speed_audio::SampleFormat::from_av(fmt) {
                if nb > 0 && channels > 0 && !ext.is_null() {
                    let width = sf.bytes();
                    let planes: Vec<&[u8]> = if planar {
                        (0..channels).map(|c| std::slice::from_raw_parts(*ext.add(c), nb * width)).collect()
                    } else {
                        vec![std::slice::from_raw_parts(*ext, nb * width * channels)]
                    };
                    let mask = frame_channel_mask(f);
                    let stereo = crate::speed_audio::downmix_by_mask(sf, planar, mask, channels, nb, &planes);
                    aus.extend(self.audio.push(pts_ns, rate, &stereo));
                }
            }
            av_frame_unref(self.frame);
        }
        aus
    }
}

impl Drop for SpeedDecoder {
    fn drop(&mut self) {
        unsafe {
            let mut c = self.ctx;
            avcodec_free_context(&mut c);
            let mut f = self.frame;
            av_frame_free(&mut f);
        }
    }
}
