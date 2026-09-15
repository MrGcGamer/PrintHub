# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.96.1

# Cross-compiles on the build host, so an arm64 image never runs rustc under emulation.
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-slim-trixie AS build
ARG ZIG_VERSION=0.16.0
ARG CARGO_ZIGBUILD_VERSION=0.23.4
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
 && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/zig-$(uname -m)-linux-${ZIG_VERSION}.tar.xz" \
    | tar -xJ -C /usr/local \
 && ln -s "/usr/local/zig-$(uname -m)-linux-${ZIG_VERSION}/zig" /usr/local/bin/zig
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install --locked "cargo-zigbuild@${CARGO_ZIGBUILD_VERSION}" \
 && rustup target add aarch64-unknown-linux-musl x86_64-unknown-linux-musl
WORKDIR /src
COPY . .
ARG TARGETARCH
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,id=printhub-target-${TARGETARCH} \
    case "$TARGETARCH" in \
      arm64) target=aarch64-unknown-linux-musl ;; \
      amd64) target=x86_64-unknown-linux-musl ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && SQLX_OFFLINE=true cargo zigbuild --release --locked -p printhub --target "$target" \
 && cp "target/$target/release/printhub" /printhub

# Unpacks the AppImage's squashfs without executing it, so this works for any target on any host.
FROM --platform=$BUILDPLATFORM debian:trixie-slim AS slicer
ARG ORCA_VERSION=2.4.2
ARG ORCA_SHA256_ARM64=e1a07275a25f176626c55a5df39e91bc4476d8c28ee4a3192ff758e29dd5c3ba
ARG ORCA_SHA256_AMD64=d12fb8c8eac1aecd2dfb6377acd48f994f8fa439ed5292fa532dd82880f029fd
ARG TARGETARCH
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl squashfs-tools \
 && rm -rf /var/lib/apt/lists/*
RUN case "$TARGETARCH" in \
      arm64) asset=Ubuntu2404_aarch64_V${ORCA_VERSION}; sha=$ORCA_SHA256_ARM64 ;; \
      amd64) asset=Ubuntu2404_V${ORCA_VERSION}; sha=$ORCA_SHA256_AMD64 ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && curl -fsSL -o /orca.AppImage \
    "https://github.com/OrcaSlicer/OrcaSlicer/releases/download/v${ORCA_VERSION}/OrcaSlicer_Linux_AppImage_${asset}.AppImage" \
 && echo "$sha  /orca.AppImage" | sha256sum -c - \
 # The squashfs starts where the ELF runtime's section header table ends:
 # e_shoff (8 bytes at 0x28) + e_shentsize (2 at 0x3A) * e_shnum (2 at 0x3C).
 && shoff=$(od -An -t u8 -j 40 -N 8 /orca.AppImage | tr -d ' ') \
 && shentsize=$(od -An -t u2 -j 58 -N 2 /orca.AppImage | tr -d ' ') \
 && shnum=$(od -An -t u2 -j 60 -N 2 /orca.AppImage | tr -d ' ') \
 && unsquashfs -q -no-progress -d /opt/orcaslicer -o $((shoff + shentsize * shnum)) /orca.AppImage \
 && rm /orca.AppImage

FROM debian:trixie-slim
# The libraries OrcaSlicer links against but does not bundle in lib/orca-runtime.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
    tzdata \
    libatk1.0-0t64 libcairo-gobject2 libcairo2 libdbus-1-3 libegl1 libfontconfig1 \
    libgdk-pixbuf-2.0-0 libgl1 libglib2.0-0t64 libglu1-mesa libglx0 libgstreamer-plugins-base1.0-0 \
    libgstreamer1.0-0 libgtk-3-0t64 libharfbuzz0b libice6 libjavascriptcoregtk-4.1-0 libopengl0 \
    libpango-1.0-0 libpangocairo-1.0-0 libpangoft2-1.0-0 libsecret-1-0 libsm6 libsoup-3.0-0 \
    libwayland-client0 libwayland-egl1 libwayland-server0 libwebkit2gtk-4.1-0 libx11-6 libxext6 \
    libxkbcommon0 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --no-create-home printhub \
 && install -d -o printhub -g printhub /data
COPY --from=slicer /opt/orcaslicer /opt/orcaslicer
COPY --from=build /printhub /usr/local/bin/printhub
# What the AppImage's own launcher sets up; LC_ALL=C is its workaround for locale segfaults.
ENV LD_LIBRARY_PATH=/opt/orcaslicer/lib/orca-runtime:/opt/orcaslicer/bin \
    LC_ALL=C
USER printhub
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD ["printhub", "healthcheck"]
ENTRYPOINT ["printhub"]
CMD ["serve"]
