static const AVInputFormat * const demuxer_list[] = {
    &ff_h264_demuxer,
    &ff_hevc_demuxer,
    &ff_matroska_demuxer,
    &ff_mov_demuxer,
    &ff_mpegts_demuxer,
    NULL };
