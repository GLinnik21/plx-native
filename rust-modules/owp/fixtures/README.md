# Open Watch Party fixtures

Copied unchanged from [edde746/openwatchparty](https://github.com/edde746/openwatchparty) at revision
`d8c3c2abc753c0486c955f39e020258c166b93d8`, under the Apache License 2.0 in `LICENSE`. The tests in `src/vectors.rs` read them;
`relays.json` is also the public relay list the crate embeds.

| file | upstream path | sha256 |
|---|---|---|
| `LICENSE` | `LICENSE` | `c95bae1d1ce0235ecccd3560b772ec1efb97f348a79f0fbe0a634f0c2ccefe2c` |
| `examples/invalid/descriptor.duplicate-key-in-ext.json` | `spec/examples/invalid/descriptor.duplicate-key-in-ext.json` | `d2d6cf226b3d3c64af3726bda357dfbab01ec12a2f0e4864afd5192ed1144bd8` |
| `examples/invalid/descriptor.duration-past-max.json` | `spec/examples/invalid/descriptor.duration-past-max.json` | `76f6f152c90625f5c983195026abfadd6a08435180dcee2c4bf14c42e8e6c02d` |
| `examples/invalid/descriptor.duration-zero.json` | `spec/examples/invalid/descriptor.duration-zero.json` | `d6324ae05e763d8834ef7162d84a57642de261d654f52837b6bcb80f4a31eeb5` |
| `examples/invalid/descriptor.ext-key-lone-surrogate.json` | `spec/examples/invalid/descriptor.ext-key-lone-surrogate.json` | `4f4ad891884bb556daafb8bebfa4d87e395700d3afc7e0d13d0d3369bfcfdaac` |
| `examples/invalid/descriptor.ext-key-not-reverse-dns.json` | `spec/examples/invalid/descriptor.ext-key-not-reverse-dns.json` | `1564bc07ba18ec92c49473d511d93bd02084d390ea604fbbd84bf8ccc19967e1` |
| `examples/invalid/descriptor.ext-too-large.json` | `spec/examples/invalid/descriptor.ext-too-large.json` | `20405b9643727bf1673f1ef2209ee46f5f9ecd9f442180cf5513369c1e445188` |
| `examples/invalid/descriptor.file-name-backslash.json` | `spec/examples/invalid/descriptor.file-name-backslash.json` | `0a1e6dc3134297a85df440e5eaa54af0affde1c9fce1ff07f902612f6810d7d5` |
| `examples/invalid/descriptor.file-name-dot.json` | `spec/examples/invalid/descriptor.file-name-dot.json` | `490999a5ae8b6f0af0a51a0d488be849a4a1e3761bfab6f8094a5dc2e6205258` |
| `examples/invalid/descriptor.file-name-dotdot.json` | `spec/examples/invalid/descriptor.file-name-dotdot.json` | `640a360b5fb63f0a902c24ab9932597f2b419214466d7693c6ef7bf704bada7e` |
| `examples/invalid/descriptor.file-name-nul.json` | `spec/examples/invalid/descriptor.file-name-nul.json` | `9582a9157529f4700a3a5776d91a224a4d9c9a10e7f44417343fc74f6ed4e94e` |
| `examples/invalid/descriptor.file-name-rlo.json` | `spec/examples/invalid/descriptor.file-name-rlo.json` | `4a72899dbe4c2f14002869503522ee83675cfb1d3939a226adb924bd42cc9fd5` |
| `examples/invalid/descriptor.file-name-slash.json` | `spec/examples/invalid/descriptor.file-name-slash.json` | `fa143a5eac23548733cec83a32da6ebcac5fd1a2c1557dc7ce36762d186a32b4` |
| `examples/invalid/descriptor.id-number.json` | `spec/examples/invalid/descriptor.id-number.json` | `6013045aea7808b7cb3c42b8bcb72bdb5fd05516438bad7757e85c1ca064a467` |
| `examples/invalid/descriptor.kind-unknown.json` | `spec/examples/invalid/descriptor.kind-unknown.json` | `0aa87033e3ea8f78aceae43a29d6265551baf220ad975a3466c2023b9f0386e3` |
| `examples/invalid/descriptor.oshash-uppercase.json` | `spec/examples/invalid/descriptor.oshash-uppercase.json` | `d33efd0c332b444c80333f043451a328014682b99fccdacb5a09c0cb8887839a` |
| `examples/invalid/descriptor.series-title-left-to-right-mark.json` | `spec/examples/invalid/descriptor.series-title-left-to-right-mark.json` | `4549a27f8cbbb8a3c14b109763023499ac02c69ab6c1d6b5efe43b9db5c4c948` |
| `examples/invalid/descriptor.series-without-title.json` | `spec/examples/invalid/descriptor.series-without-title.json` | `79078c631e6ccf93d04e09901ef58503732e50b38374b23a5bf75cca44555124` |
| `examples/invalid/descriptor.title-bidi-isolate.json` | `spec/examples/invalid/descriptor.title-bidi-isolate.json` | `1792a9ab863650f93179f19abb9b62c4ed08a234b3467216b61da3cd04c70e2d` |
| `examples/invalid/descriptor.title-c1-control.json` | `spec/examples/invalid/descriptor.title-c1-control.json` | `2bc257118427e0e2e2576cc272f17b4e83a3fdbfb648dca476a8e422cf3e03e3` |
| `examples/invalid/descriptor.title-empty.json` | `spec/examples/invalid/descriptor.title-empty.json` | `ef113cc8c8ef5ef9cf77a177886b519c4a39e009dbfca177fb1cf068ad576f9a` |
| `examples/invalid/descriptor.title-newline.json` | `spec/examples/invalid/descriptor.title-newline.json` | `28dbdf1969040cc1a2d8640b592302f77a4216079915186ae763f660e1e67927` |
| `examples/invalid/descriptor.too-large.json` | `spec/examples/invalid/descriptor.too-large.json` | `8862a3dff370a2fe23f353d444e6b22f6c3397436e466ca4ed5101d30bd01057` |
| `examples/invalid/setMedia.duplicate-escaped-key.json` | `spec/examples/invalid/setMedia.duplicate-escaped-key.json` | `eb6417a0a43cc00dfb4e4a2ddabda0e214530448e06b74557cc2dafb5cc5da46` |
| `examples/invalid/setMedia.duplicate-key-in-media.json` | `spec/examples/invalid/setMedia.duplicate-key-in-media.json` | `7049622eec4de619744cd8539e0d9fc5763b987d3e2db01ae8c43235227b2614` |
| `examples/invalid/setMedia.duration-past-max.json` | `spec/examples/invalid/setMedia.duration-past-max.json` | `326e7c1fffac16fde6d748d76ff6ab237559cb7f5bea2a661807e251966ab984` |
| `examples/invalid/setMedia.episode-without-series.json` | `spec/examples/invalid/setMedia.episode-without-series.json` | `0c6461cff87b63ba02ed0e113b784d9c8c3bea62efbcd7335860e38e2053a050` |
| `examples/invalid/setMedia.file-name-traversal.json` | `spec/examples/invalid/setMedia.file-name-traversal.json` | `07089335ea3e7d9f128bb4c408e0f9d7cdf6fab1f8c8d1a76774d648792b10c9` |
| `examples/invalid/setMedia.media-half-emoji.json` | `spec/examples/invalid/setMedia.media-half-emoji.json` | `2697ee65e7d9aadf3bd90b59cf44317c0e463808737bb884d8940233bbcbb888` |
| `examples/invalid/setMedia.media-invalid-utf8.json` | `spec/examples/invalid/setMedia.media-invalid-utf8.json` | `65b65e8530de6a2bc89f50c7d1d54e65fb7d772aaff3bb5d9d577cc7244feeb8` |
| `examples/invalid/setMedia.media-null.json` | `spec/examples/invalid/setMedia.media-null.json` | `5cfce2d53f6f219b31a3b437126caaa7a38e7bd1e24604cc545d44cbd32f0b9d` |
| `examples/invalid/setMedia.media-too-large.json` | `spec/examples/invalid/setMedia.media-too-large.json` | `64cfe9f47273f44dd053a751b0834fb1e54b4030ac3dffe0019fae7b6c07f4f9` |
| `examples/invalid/setMedia.media-without-duration.json` | `spec/examples/invalid/setMedia.media-without-duration.json` | `21fb22618d092894b49adc6f37fd8bfac9184942ab94d3a87b36dbb2620ec954` |
| `examples/invalid/setMedia.missing-media.json` | `spec/examples/invalid/setMedia.missing-media.json` | `0ff4a7b50094446287770cb64a2c8e27e4f90e3086acca4f464ab735419556c2` |
| `examples/valid/descriptor.duration-max.json` | `spec/examples/valid/descriptor.duration-max.json` | `d0270a2eb5ef659754cdef91b9c86ad7d41799d9d20e3992b43aab194c0bd18f` |
| `examples/valid/descriptor.episode-app-ids.json` | `spec/examples/valid/descriptor.episode-app-ids.json` | `c858f11e0a8bdb130f131ec19b327f82239f1153993daacdf8c38bb1c4339d7d` |
| `examples/valid/descriptor.episode.json` | `spec/examples/valid/descriptor.episode.json` | `0a005cfc39ecc6a1173c428a2eff94c89d6d1dbb2ed6b4ea2fbee4ac16919426` |
| `examples/valid/descriptor.file-name-hidden.json` | `spec/examples/valid/descriptor.file-name-hidden.json` | `b5a48276ee7d158f3458d7e92793027488bfdfa0863dee5c0f87b66c60b63989` |
| `examples/valid/descriptor.file-name-leading-dots.json` | `spec/examples/valid/descriptor.file-name-leading-dots.json` | `d19fdff0de81718c6ca5d615b70c6cf90c533eb2f26aa5edf0062464468da4d7` |
| `examples/valid/descriptor.minimal.json` | `spec/examples/valid/descriptor.minimal.json` | `3b5b251f9e3aabd19577dc286daa7bb78b20d015bce4b6e418db4a66effd1120` |
| `examples/valid/descriptor.movie.json` | `spec/examples/valid/descriptor.movie.json` | `2faaed302935c6448213c30adfcc60a47124b4525ebb5f3038234b78bf8756c4` |
| `examples/valid/descriptor.title-emoji-zwj.json` | `spec/examples/valid/descriptor.title-emoji-zwj.json` | `34a6744cbd0901b950fc2d5ff0123ee3ce43360905f25c6be95ebb530fd2f30e` |
| `examples/valid/descriptor.title-rtl-script.json` | `spec/examples/valid/descriptor.title-rtl-script.json` | `bb084aafc878e2b21de9a345d6bd43b7f6782ddb08b4ad593d00ebfff5d23279` |
| `examples/valid/descriptor.unknown-namespace.json` | `spec/examples/valid/descriptor.unknown-namespace.json` | `a6e9ba3eaa859f531809403a53a5999853b50c46eb81066ea84a1150f92385da` |
| `examples/valid/descriptor.video.json` | `spec/examples/valid/descriptor.video.json` | `60eca50283e82c98b7a118764b7235ebdc6a26964791226375c2b81b4796f0ac` |
| `examples/valid/setMedia.episode.json` | `spec/examples/valid/setMedia.episode.json` | `6a99452e366525d3226e14b28f3df4325a7d1232312b515dfa33f44c1d41e82b` |
| `examples/valid/setMedia.movie-at-position.json` | `spec/examples/valid/setMedia.movie-at-position.json` | `d6130b2928ccf0137e40d2764d846f1c88f964e19291e8a7cd094f4cca35e702` |
| `relays.json` | `relays.json` | `755585e878bb9290b0461ecd9e863e79e85cb09d962bd00e37ada9e201e6a1a3` |
| `vectors/clock-sync.json` | `conformance/vectors/clock-sync.json` | `3bd79ab3481c268322def5940137258c8e9a2e2788a12f176d6d04ed36d52991` |
| `vectors/code-routing.json` | `conformance/vectors/code-routing.json` | `11192c267fe46165d7939afd089a140c069232149a77e3d90afdd55da3cb78e1` |
| `vectors/media-check.json` | `conformance/vectors/media-check.json` | `c20ac0be6676e073f2b88f0475e0335862db88ee4361961ff0f4e746842f2a11` |
| `vectors/names.json` | `conformance/vectors/names.json` | `9193de5a447f3cba67f2aaffb48efbe1932df17030f14bce1bf2b8dd72eb7bf2` |
| `vectors/relay-list.json` | `conformance/vectors/relay-list.json` | `d8f962bbb6e129ce2a71dcc2e893090c0a913475e04a877a276bbc77e930686e` |
| `vectors/room-check.json` | `conformance/vectors/room-check.json` | `6b903bc22e8f861d924e57b8508e79b3924a1d72050f65867617308537498b70` |
| `vectors/same-title.json` | `conformance/vectors/same-title.json` | `2b449084b1a459a0bdc1ef20bddec4302597bd3bf552d37cf6ca08fff47be658` |
| `vectors/timeline.json` | `conformance/vectors/timeline.json` | `82095be784247d104cc2cc845a17ddaa937e0270723af3e0aab977e2545566cd` |
