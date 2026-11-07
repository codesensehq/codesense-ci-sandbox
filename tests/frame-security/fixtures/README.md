Small, locally generated native-media fixtures, with no external content:

- `silence.wav`: 0.25 seconds of silent mono PCM16 at 8 kHz.
- `black.mp4`: ten 16×16 black H.264 frames at 10 fps, encoded with macOS AVFoundation.

The browser tests load these with native audio/video elements and wait for metadata. They do not need autoplay or an app build.
