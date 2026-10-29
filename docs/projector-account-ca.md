# Older webOS certificate compatibility

Verified on an LG HU80KS-NA running webOS 3.7.0, firmware 03.00.11, with libcurl 7.50.1/OpenSSL 1.0.2k. Other firmware versions have not been device-tested for this change.

Plex account HTTPS requests whose parsed hostname is exactly plex.tv use the shipped Mozilla public CA bundle when available. Both public request entry points share this policy. The bundle comes from https://curl.se/ca/cacert.pem (retrieved 2026-10-22). Peer and hostname verification remain enabled; a missing bundle preserves the original device-store behavior.

This firmware also reports generic X509 verification result 1 when the device store cannot validate a Plex server certificate. On plex.direct hosts only, that result now permits one fully verified retry against the existing shipped public roots. It does not establish a missing-issuer diagnosis or relax chain, date or hostname validation. Existing date-pin policy is unchanged.

Device verification confirmed account sign-in, persisted session and certificate-verified HTTPS access to the owned Plex server. An attempted playback failed because the server media share was unmounted; that attempt does not establish encrypted media playback compatibility. Network tests and shipping configuration passed. The full host suite was blocked by missing SDL2 on the build host.
