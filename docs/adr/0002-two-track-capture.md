# Capture You and Others as two separate tracks

We record the default microphone and the default sink monitor as two separate PipeWire streams and never mix them down. That gives "You vs Others" speaker separation without a diarization model. The cost is that without headphones the mic picks up remote voices, so the You Track gets PipeWire echo-cancel and a text-similarity dedupe drops You speech that duplicates Others speech at the same time. Diarizing individual remote Speakers is deferred, but Segments carry an optional Speaker index from the start so adding it later changes no schema.

The dedupe compares words, not whole Segments: a You word is dropped when a similar Others word is centred within about a second of it, provided it is part of a run of such words or stands alone. Genuine interjections said over echo ("Great", "Okay") survive even when they fall inside an echoed sentence.
