set -e
apt-get update -qq >/dev/null
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq meson ninja-build python3-mako python3-yaml python3-packaging bison flex pkg-config gcc g++ libdrm-dev zlib1g-dev libexpat1-dev glslang-tools libelf-dev >/dev/null
cd /src/mesa-25.2.6
meson setup /src/build -Dgallium-drivers=freedreno -Dvulkan-drivers= -Dtools=freedreno -Dplatforms= -Dglx=disabled -Degl=disabled -Dgbm=disabled -Dllvm=disabled -Dvalgrind=disabled -Dlibunwind=disabled -Dbuild-tests=false -Dfreedreno-kmds=msm > /src/meson.log 2>&1
ninja -C /src/build src/freedreno/isa/ir3-disasm > /src/ninja-disasm.log 2>&1 || true
ninja -C /src/build > /src/ninja.log 2>&1 || true
ls /src/build/src/freedreno/ir3 /src/build/src/freedreno/isa
