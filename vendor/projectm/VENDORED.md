# libprojectM, vendored

This tree is a copy of upstream libprojectM, committed into projectmp4 rather
than referenced as a git submodule. A clone of this repository builds without
`git submodule update`, and the local changes below travel with it — as a
submodule they lived only in a working tree and any fresh clone silently lost
them.

## Provenance

| | |
|---|---|
| Upstream | <https://github.com/projectM-visualizer/projectm> |
| Commit | `eae22de40144ee8ded92f3c04b93a4c2f82adcc0` (v3.1.8-765-geae22de40) |
| `vendor/projectm-eval` | <https://github.com/projectM-visualizer/projectm-eval> at `22fb0cfd8f2dfbcd2b68f2443e7f44e19b32c09a` (v1.0.7) |
| Vendored | 2026-11-08 |

`vendor/projectm-eval` was itself a submodule of upstream and is flattened into
this tree at the commit above.

## Licence

libprojectM is **LGPL-2.1** (`LICENSE.txt`, `COPYING`); `vendor/projectm-eval`
is **MIT** (`vendor/projectm-eval/LICENSE.md`). Both licence files are kept.
The LGPL asks that modifications to the library be carried as such: they are
listed below and are visible in this repository's history as a commit of their
own, applied on top of a commit of the pristine upstream tree.

## What was left out

Two things, both of them bytes nothing builds from:

- `docs/web/`, 28 MB of upstream's project website (images and generated pages).
- `background.png`, 9.3 MB of upstream artwork at the tree root.

Everything else upstream tracks at that commit is here, 926 files. To recover
either, fetch the commit above from upstream.

## Local changes

### `src/libprojectM/Renderer/VertexBuffer.hpp`, `src/libprojectM/Renderer/VertexIndexArray.cpp` — orphan buffers instead of `glBufferSubData`

Both `Update()` methods took one of two paths: `glBufferSubData` when the
element count was unchanged, `glBufferData` when it had changed. The first is
the intended fast path and on Apple's Metal-backed OpenGL driver it is the slow
one by a wide margin.

A `glBufferSubData` write targets storage the GPU may still be reading for the
frame in flight. The driver cannot rename the buffer underneath us, so it
flushes the command queue and blocks for a full round trip:

```
gldBufferSubData -> GLDContextRec::flushResource -> flushContext
                 -> [_MTLCommandQueue submitCommandBuffer:] -> mach_msg2_trap
```

`glBufferData` respecifies the whole buffer, which lets the driver hand back
fresh storage and return at once — the standard orphaning idiom. Both methods
now always take that path.

Measured on an M-series Mac, `CustomShape::Draw -> Mesh::Update ->
glBufferSubData` was **64% of wall clock** on a 1080p60 render (`sample`, 20 s,
8111 of 12598 main-thread samples). Removing the stall cut a fixed 20 s
workload from 112.4 s to 55.3 s, a little over 2x.

This is a platform-specific trade, not a general improvement: on a driver that
can rename buffers, `glBufferSubData` avoids a reallocation and is the better
call. It is correct everywhere, only not always fastest.

## Re-syncing with upstream

There is no submodule to bump. Fetch the upstream commit you want, replace this
tree with it, and re-apply the changes above — each is a few lines in one
function, and each is explained here well enough to redo by hand if upstream
has moved the code.
