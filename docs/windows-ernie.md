# ERNIE-5.5-Flash-Preview on native Windows

The x64 release targets the RTX 3090 (SM 8.6) and Cascade Lake Xeons. The server uses the NVIDIA Windows driver, a local checkpoint and one request slot. The release archive contains the server with its built-in repacker and the PowerShell launcher. Install the x64 Microsoft Visual C++ v14 Redistributable if Windows reports a missing VCRUNTIME140.dll. On the first run the driver may compile its embedded CUDA modules into the driver cache.

## Start with an existing checkpoint

Extract the archive together, open PowerShell in the extracted directory and run:

```powershell
.\Start-ERNIE.ps1 -Model 'E:\Models\ERNIE-5.5-Flash-Preview'
```

The launcher selects the RTX 3090 by its NVIDIA UUID and binds the server to `127.0.0.1:8091`. The OpenAI-compatible base URL is `http://127.0.0.1:8091/v1`. Ctrl+C closes the server. To choose a GPU explicitly, pass its `nvidia-smi` index with `-Gpu`. The checkpoint can be Baidu's complete safetensors folder (including its tokenizer and index) or the first shard of Bloomery's F8M4 GGUF. On the first serving start with a vendor folder, the bundled CPU repacker writes a lossless F8M4 GGUF beside it in `<model-folder>.bloomery-F8M4`. Allow about **145 GiB of additional free disk space** for the full Baidu checkpoint. Use `-CacheDirectory 'F:\ERNIE-F8M4'` to choose another drive. Later starts reuse the completed shards. An existing F8M4 GGUF passed to `-Model` starts directly and needs no repack.

Baidu stores each expert's MXFP4 codes and scales separately. Bloomery's CPU tier reads contiguous GGUF expert blocks. The repacker rearranges the stored data and retains the MXFP4 experts, FP8 block-scaled projections, BF16 and F32 tensors. Conversion runs only during preparation. During generation, the server uses VRAM and locks its host expert pages in RAM. The source files stay in place, and the entire operation can run offline.

To request an exact context, pass `-Context`. Add `-Plan` to calculate the placement for that request on the installed card before loading. A plan uses existing GGUF shards when the cache is ready and otherwise reads the source metadata. It does not create a cache:

```powershell
.\Start-ERNIE.ps1 -Model 'E:\Models\ERNIE-5.5-Flash-Preview' -Context 128192 -Plan
```

With no explicit context, Bloomery selects a context that fits. A supplied context must fit ERNIE's on-card attention stores and the space available to the NVIDIA driver. Increasing context can displace GPU-resident experts and affect generation speed.

The Windows ERNIE planner requires the host experts to fit in RAM. The launcher populates their mapped pages and requires Windows to lock them for the lifetime of the model. Initial reads of the checkpoint are expected. A refused lock stops loading and reports the Windows error. If another model is running, unload it first so its RAM and VRAM are available.

## Send a request and read the token rates

From a second PowerShell window after the server is ready:

```powershell
$base = 'http://127.0.0.1:8091'
Invoke-RestMethod "$base/health"
$body = @{
    model = 'ernie-5.5-flash-preview'
    messages = @(@{ role = 'user'; content = 'Name the capital of Australia and the territory it is in.' })
    temperature = 0
    max_tokens = 128
    reasoning_effort = 'none'
} | ConvertTo-Json -Depth 5
$reply = Invoke-RestMethod "$base/v1/chat/completions" -Method Post -ContentType 'application/json' -Body $body
$reply.choices[0].message.content
$reply.timings
```

`prompt_per_second` measures prefill and `predicted_per_second` measures generation. Compare runs using the same checkpoint, context, input, output length and cache state. The repository's model catalogue records an upstream GPU run for the F8M4 GGUF. It lists the original Baidu folder as unverified on a card. An on-device generation and timing are needed to establish workstation performance.

## Build from source

For a native build install x64 Visual Studio C++ Build Tools and the Windows SDK, Rustup, vcpkg, the CUDA 13.3 toolkit including cuRAND and libdevice, and the full x64 LLVM 21 distribution with NVPTX, libclang, llc and llvm-link. The build tool version and installed driver are separate. The prebuilt server needs the driver; these SDKs are build dependencies.

```powershell
.\tools\windows\Build-ERNIE.ps1 -CudaPath $env:CUDA_PATH -Llc (Get-Command llc.exe).Source
```

Rust and CUDA-Oxide revisions come from the repository. The first build downloads compiler components and dependencies. The output is `target\x86_64-pc-windows-msvc\release\bloomery-serve.exe`, which the root launcher discovers automatically.
