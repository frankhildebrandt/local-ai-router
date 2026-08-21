#!/usr/bin/env bash
set -euo pipefail

# ggml-vulkan requires the glslc component. Ubuntu 22.04 main has no glslc
# package (added in 24.04); pull shaderc from LunarG's Vulkan apt repo instead.
if command -v glslc >/dev/null 2>&1; then
  glslc --version | head -1
  exit 0
fi

wget -qO- https://packages.lunarg.com/lunarg-signing-key-pub.asc \
  | sudo gpg --dearmor -o /usr/share/keyrings/lunarg.gpg
echo "deb [signed-by=/usr/share/keyrings/lunarg.gpg] https://packages.lunarg.com/vulkan/ jammy main" \
  | sudo tee /etc/apt/sources.list.d/lunarg-vulkan-jammy.list >/dev/null
sudo apt-get update
sudo apt-get install -y shaderc
command -v glslc
glslc --version | head -1
