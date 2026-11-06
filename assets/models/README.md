# Section model weights

`harmonix-fold0-f32.gguf` — weights for section labelling (intro/verse/chorus/drop).

| | |
| --- | --- |
| Format | GGUF v3 / F32 / `harmonix-f32-v1`, 491 tensors |
| sha256 | `f9b48befd3b611d8d11285ad8602ca28cd30457bfdc58940e0bf88ea858cdfe6` |
| Size | 1246176 bytes |
| Upstream model | https://huggingface.co/taejunkim/allinone, revision `379e5fd010b3fdd0ee8381ff8cbcfa51d70b5c19` (MIT) |
| MLX conversion | https://github.com/ssmall256/all-in-one-mlx, revision `8f67ccd0f6460a1e02bc1c68d6d1c3572a16e368`, `mlx-weights/harmonix-fold0_mlx.npz`, sha256 `2d04d162842b048c34d9476c061f2612e4b9cd339d264aa2fb6ec88c59fde1c0` |

Read by `src/audio/sections/gguf.rs`; names and shapes pinned in `src/audio/sections/schema.rs`.
