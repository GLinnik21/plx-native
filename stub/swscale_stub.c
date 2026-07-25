// Link-time stub for the TV's real libswscale.so.4 (FFmpeg 3.3). SONAME
// libswscale.so.4. Real bodies come from the device at runtime. Used only by the
// dev capture stream's MPEG1 encoder (RGBA -> YUV420P conversion).
void *sws_getContext(int sw, int sh, int sf, int dw, int dh, int df, int flags,
                     void *srcFilter, void *dstFilter, const double *param) {
    (void)sw; (void)sh; (void)sf; (void)dw; (void)dh; (void)df; (void)flags;
    (void)srcFilter; (void)dstFilter; (void)param; return 0;
}
int sws_scale(void *c, const void *srcSlice, const int *srcStride, int srcSliceY,
              int srcSliceH, void *dst, const int *dstStride) {
    (void)c; (void)srcSlice; (void)srcStride; (void)srcSliceY; (void)srcSliceH;
    (void)dst; (void)dstStride; return 0;
}
void sws_freeContext(void *c) { (void)c; }
