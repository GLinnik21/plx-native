// Link-time stub for the TV's real libcurl.so.5 (present at /usr/lib on the device).
// Carries the SONAME libcurl.so.5 (see Makefile) so DT_NEEDED matches; at runtime the
// device's real library is loaded and these empty bodies are never executed. Only the
// symbols the app actually calls need to appear here (name-only match; signatures are
// irrelevant on the host since the bodies never run). Used by rust net.rs for the plex.tv
// HTTPS account/login calls — the local PMS still uses the plain-HTTP stream.rs socket.
int   curl_global_init(long flags) { (void)flags; return 0; }
void *curl_easy_init(void) { return 0; }
int   curl_easy_setopt(void *h, int opt, ...) { (void)h; (void)opt; return 0; }
int   curl_easy_perform(void *h) { (void)h; return 0; }
int   curl_easy_getinfo(void *h, int info, ...) { (void)h; (void)info; return 0; }
void  curl_easy_cleanup(void *h) { (void)h; }
void *curl_slist_append(void *list, const char *s) { (void)list; (void)s; return 0; }
void  curl_slist_free_all(void *list) { (void)list; }
