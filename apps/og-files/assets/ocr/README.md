# OCR models (embedded)

`src/viewers/pdf/tools/ocr.rs` compiles these two files into the binary
with `include_bytes!` and runs them with the pure-Rust `ocrs` 0.13 engine
on the `rten` 0.26 runtime. Nothing is downloaded at build or run time.

| File | Size (bytes) | sha256 |
|---|---|---|
| `text-detection.rten` | 2,510,284 | `f15cfb56bd02c4bf478a20343986504a1f01e1665c2b3a0ad66340f054b1b5ca` |
| `text-recognition.rten` | 9,716,568 | `e484866d4cce403175bd8d00b128feb08ab42e208de30e42cd9889d8f1735a6e` |

**Source:** the official ocrs model files, the ones `ocrs-cli` uses by
default (it fetches the `.onnx` twins of these from the same bucket):

- https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.rten
- https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.rten

Downloaded 2026-10-16. Training code: https://github.com/robertknight/ocrs-models.
Model card: https://huggingface.co/robertknight/ocrs.

**Version:** the default models for ocrs 0.13.x (ocrs-cli 0.13.1). The
recognition model's alphabet is ocrs' `DEFAULT_ALPHABET` (printable ASCII;
the slot meant for "€" decodes as "E").

**License:** the model weights are published under **CC BY-SA 4.0**
(Hugging Face model card; trained on Google's HierText dataset, also
CC BY-SA 4.0). Creative Commons lists GPLv3 as a compatible license for
CC BY-SA 4.0 adaptations (https://creativecommons.org/share-your-work/licensing-considerations/compatible-licenses),
so shipping them inside this GPL-3.0-or-later program is permitted with
attribution. Attribution: "ocrs text detection / recognition models" by
Robert Knight, https://github.com/robertknight/ocrs-models, CC BY-SA 4.0;
unmodified. The `ocrs` and `rten` crates themselves are MIT OR Apache-2.0.

To update: download the new files, replace them here, update the table
(sizes and `sha256sum`), and run `cargo test --release -p og-files ocr`.
