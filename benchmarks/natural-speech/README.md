# Natural speech negative controls

This complements the synthetic names benchmark with 73 public LibriSpeech clean
validation recordings. This small subset has **one speaker and three chapters**;
it tests ordinary speech under irrelevant vocabulary, not speaker diversity or
conversational accuracy. Source: `hf-internal-testing/librispeech_asr_dummy`,
`clean/validation`; underlying LibriSpeech license: CC BY 4.0.

`python3 benchmarks/natural-speech/prepare.py /tmp/localwispr-natural-validation`
downloads public audio, converts it using ffmpeg, and freezes paired requests and
SHA-256 hashes. It requires Python's standard library and ffmpeg. Audio and model
files stay outside the repository. Context-off and fixed distractor conditions
use identical audio/settings; distractors are frozen before any decoding.

Build the committed frozen recognition harness after `cargo build --release --workspace`, then run the adapter and scorer:

```sh
python3 benchmarks/recognition/build.py
python3 benchmarks/natural-speech/run.py /tmp/localwispr-natural-validation/requests.jsonl /tmp/localwispr-natural-validation/replay.jsonl
python3 benchmarks/natural-speech/score.py /tmp/localwispr-natural-validation/replay.jsonl
```

The adapter checks audio hashes, uses Fast decoding with VAD (the recorded
experiment settings), translates the request/response formats, and refuses to
overwrite results. `--harness /path/to/recognition-harness` selects another source
snapshot built with the same protocol. The default harness uses the historical
`benchmarks/recognition/frozen` sources, not the current production checkout.
Models default to the app's installed model directory; set
`LOCALWISPR_BENCH_MODELS` to override it. Scoring reports WER, paired changes, and
exact transcripts. Reference wording is not used to create vocabulary hints.
