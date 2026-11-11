# syntax=docker/dockerfile:1
#
# Minimal multi-stage image for Ultimaker CuraEngine.
#
#   docker build -t cura-engine:5.14.0 -f docker/cura-engine/Dockerfile docker/cura-engine
#   docker run --rm cura-engine:5.14.0 CuraEngine help
#
# Build stage compiles CuraEngine from source (glibc Ubuntu, so the prebuilt
# binaries Conan downloads -- e.g. its cmake tool -- run natively, no shims).
# Runtime stage is ubuntu:24.04 with only the CuraEngine binary plus the
# shared-library closure it actually needs, collected from the build stage's
# `ldd` output. No toolchain, Python, or network at runtime.
#
# ubuntu:24.04 (noble) satisfies the upstream requirements natively:
#   Python 3.12, gcc/g++ 13, CMake 3.28, Ninja, make.

ARG DEBIAN_VERSION=stable

FROM debian:${DEBIAN_VERSION} AS builder
ARG DEBIAN_VERSION
# Keep these in sync with the docs; override at build time if needed:
#   docker build --build-arg CURA_ENGINE_VERSION=5.14.0 ...
ARG CURA_ENGINE_VERSION=5.14.0
ARG CONAN_VERSION=2.34.0
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git \
        curl \
        ca-certificates \
        python3 \
        python3-pip \
        gcc \
        g++ \
        make \
        cmake \
        ninja-build \
        pkg-config \
        perl \
        patch \
    && rm -rf /var/lib/apt/lists/*

# foonathan-lexy (an engine dependency) pulls an unpinned `cmake/[>=3.18 <4]`
# tool requirement that Conan resolves to an ancient, prebuilt 3.25.1, which is
# too old for the other recipes in the graph. Force all build tooling to a
# modern 3.31.x via the `default` profile (included by the Ultimaker
# cura.jinja / cura_build.jinja profiles installed above).
RUN python3 -m pip install --break-system-packages --no-cache-dir "conan==${CONAN_VERSION}" \
    && conan config install https://github.com/ultimaker/conan-config.git \
    && conan profile detect --force \
    && printf '\n[replace_tool_requires]\ncmake/*: cmake/[>=3.31 <4]\n' >> /root/.conan2/profiles/default \
    && cat /root/.conan2/profiles/default

WORKDIR /src
# Shallow clone of the pinned tag. Building from a tag makes the image reproducible.
RUN git clone --depth 1 --branch "${CURA_ENGINE_VERSION}" \
        https://github.com/Ultimaker/CuraEngine.git .

RUN conan install . --build=missing --update

# The conan-generated preset contains the real configuration. See
# https://github.com/Ultimaker/CuraEngine/wiki/Building-CuraEngine-From-Source
RUN cmake --preset conan-release \
    && cmake --build --preset conan-release --parallel "$(nproc)"

# Collect the engine binary plus every shared library Conan produced
# (e.g. libarcus); they become the runtime closure. System libs
# (libstdc++/libgcc/libc) come from the runtime stage itself. RPATHs baked in
# during the build point into the discarded cache, so an `ldd` outside the
# build would print "not found" -- hence copying the cache verbatim instead.
RUN mkdir -p /out/lib \
    && ENGINE_BIN="$(find /src -type f -name CuraEngine -perm -u+x | grep -v tests | head -n 1)" \
    && test -n "${ENGINE_BIN}" \
    && install -Dm755 "${ENGINE_BIN}" /out/CuraEngine \
    && find /root/.conan2 -type f \( -name '*.so' -o -name '*.so.*' -o -type l \) \
        ! -name 'libstdc++*' ! -name 'libgcc*' ! -name 'libc.so*' ! -name 'libm.so*' \
        -exec cp -a {} /out/lib/ \; \
    && find /out -mindepth 1 | sort

FROM debian:${DEBIAN_VERSION}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libc6 \
        libgcc-s1 \
        libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

# The first argument in `docker run <image> CuraEngine ...` is the command
# itself, so the image must not set an entrypoint. The default command prints
# help. mcp-cura invokes `<image> CuraEngine slice ...`.
COPY --from=builder /out/CuraEngine /usr/bin/CuraEngine
COPY --from=builder /out/lib/ /usr/lib/x86_64-linux-gnu/
RUN ldconfig

# CuraEngine resolves machine definitions (and their inheritance chain) at
# runtime; the tools and environment needed for slicing a plain STL live
# entirely inside this image. Profile definitions are expected from the host
# via a mount -- see docs/cura-engine-docker.md.
WORKDIR /work
CMD ["CuraEngine", "help"]
