#!/usr/bin/env pwsh
$ErrorActionPreference = 'Stop'

$short = '12.4'
$root = "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v$short"

New-Item -ItemType Directory -Force -Path $root | Out-Null
Set-Location $env:RUNNER_TEMP

$archives = @(
  @{ Component = 'cuda_cudart'; Name = 'cuda_cudart-windows-x86_64-12.4.127-archive.zip' },
  @{ Component = 'cuda_nvcc'; Name = 'cuda_nvcc-windows-x86_64-12.4.131-archive.zip' },
  @{ Component = 'cuda_nvrtc'; Name = 'cuda_nvrtc-windows-x86_64-12.4.127-archive.zip' },
  @{ Component = 'libcublas'; Name = 'libcublas-windows-x86_64-12.4.5.8-archive.zip' },
  @{ Component = 'cuda_nvtx'; Name = 'cuda_nvtx-windows-x86_64-12.4.127-archive.zip' },
  @{ Component = 'cuda_profiler_api'; Name = 'cuda_profiler_api-windows-x86_64-12.4.127-archive.zip' },
  @{ Component = 'visual_studio_integration'; Name = 'visual_studio_integration-windows-x86_64-12.4.127-archive.zip' },
  @{ Component = 'cuda_nvprof'; Name = 'cuda_nvprof-windows-x86_64-12.4.127-archive.zip' },
  @{ Component = 'cuda_cccl'; Name = 'cuda_cccl-windows-x86_64-12.4.127-archive.zip' }
)

foreach ($entry in $archives) {
  $url = "https://developer.download.nvidia.com/compute/cuda/redist/$($entry.Component)/windows-x86_64/$($entry.Name)"
  curl.exe -L --fail -o $entry.Name $url
  Expand-Archive -Path $entry.Name -DestinationPath $root -Force
  $folder = Join-Path $root ($entry.Name -replace '\.zip$', '')
  if (Test-Path $folder) {
    Copy-Item -Path (Join-Path $folder '*') -Destination $root -Recurse -Force
  }
}

"CUDA_PATH=$root" | Out-File $env:GITHUB_ENV -Append -Encoding utf8
"CUDA_PATH_V12_4=$root" | Out-File $env:GITHUB_ENV -Append -Encoding utf8
"$root\bin" | Out-File $env:GITHUB_PATH -Append -Encoding utf8
if (-not (Test-Path "$root\lib\x64\cublas.lib")) {
  throw "missing $root\lib\x64\cublas.lib after CUDA redist install"
}
