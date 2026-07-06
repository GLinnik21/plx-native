// Link-time stub for the TV's real libavformat.so.57 (FFmpeg 3.4). Carries the
// SONAME libavformat.so.57 (see Makefile) so DT_NEEDED matches; at runtime the
// device's real library is loaded and these empty bodies are never executed.
// Only the symbols the app actually calls need to appear here (name-only match).
unsigned avformat_version(void) { return 0; }
void avformat_network_init(void) {}
void avformat_network_deinit(void) {}
int avformat_open_input(void **ps, const char *url, void *fmt, void **options) { (void)ps; (void)url; (void)fmt; (void)options; return 0; }
void avformat_close_input(void **s) { (void)s; }
int avformat_find_stream_info(void *ic, void **options) { (void)ic; (void)options; return 0; }
int av_read_frame(void *s, void *pkt) { (void)s; (void)pkt; return 0; }
int av_seek_frame(void *s, int stream_index, long long timestamp, int flags) { (void)s; (void)stream_index; (void)timestamp; (void)flags; return 0; }
int avformat_seek_file(void *s, int stream_index, long long min_ts, long long ts, long long max_ts, int flags) { (void)s; (void)stream_index; (void)min_ts; (void)ts; (void)max_ts; (void)flags; return 0; }
int av_find_best_stream(void *ic, int type, int wanted, int related, void **decoder_ret, int flags) { (void)ic; (void)type; (void)wanted; (void)related; (void)decoder_ret; (void)flags; return 0; }
void *avformat_alloc_context(void) { return 0; }
