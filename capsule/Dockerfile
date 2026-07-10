# Hellbox capsule: native aarch64 Chocolate Doom streamed to the browser.
# Digest-pinned so the base can't be silently retargeted (everything else in this
# file is already SHA256-verified). Update the digest deliberately when bumping.
FROM public.ecr.aws/lambda/microvms:al2023-minimal@sha256:05cb9b38d841e7ff1b693dc9e894909612f340bf99ec97d426e8000a5bbe96c3

ARG FFMPEG_RELEASE=autobuild-2026-06-26-13-36
ARG FFMPEG_ASSET=ffmpeg-n8.1.2-linuxarm64-gpl-8.1.tar.xz
ARG FFMPEG_SHA256=42be928da25b3e1f7dc271b78a0087dbfcf4738df1d7bf46d158a86eedb285da
ARG NOVNC_VERSION=1.5.0
ARG NOVNC_SHA256=6a73e41f98388a5348b7902f54b02d177cb73b7e5eb0a7a0dcf688cc2c79b42a
# SDL2 stack + Chocolate Doom, compiled once in CI inside this exact base image
# (.github/workflows/capsule-prebuilt.yml, attestation-signed). Downloading it
# here instead of compiling cuts the cloud image build by several minutes.
ARG PREBUILT_RELEASE=capsule-prebuilt-1
ARG PREBUILT_ASSET=capsule-prebuilt-aarch64.tar.gz
ARG PREBUILT_SHA256=acb6a458a36e2a532be8698e64b13121b1aaee96cff302478f278d8a1c7ca9ee
ARG DOOM1_WAD_COMMIT=fcf22d9773c62835724f2d25c40933ef051f6600
ARG DOOM1_WAD_SHA256=1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771

# Display, audio, build/runtime deps.
# findutils is explicit: the compiler toolchain used to drag it in transitively,
# and run_app.sh's WAD discovery dies without `find`.
RUN dnf install -y tigervnc-server xterm xsetroot python3.11 python3.11-pip tar gzip xz binutils which procps-ng \
        shadow-utils pulseaudio pulseaudio-utils findutils \
    && dnf clean all
# SDL/X/audio runtime libs.
RUN dnf install -y glib2 libX11 libXext libXrender libXrandr libXcursor libXfixes \
        libXi libXcomposite libXinerama libxcb libxkbcommon libxkbcommon-x11 \
        libXScrnSaver freetype fontconfig libjpeg-turbo \
    && dnf clean all
RUN dnf install -y alsa-lib libusbx systemd-libs gnutls mesa-libGL mesa-libGLU \
        mesa-dri-drivers pulseaudio-libs ncurses-libs krb5-libs || true
COPY requirements.txt /tmp/capsule-requirements.txt
RUN python3.11 -m pip install --no-cache-dir --only-binary=:all: --require-hashes \
        -r /tmp/capsule-requirements.txt \
    && rm -f /tmp/capsule-requirements.txt

# Pinned ffmpeg build with libopus.
RUN set -e; \
    cd /tmp; \
    curl -fsSL --retry 3 --max-time 240 -o btbn.tar.xz \
      "https://github.com/BtbN/FFmpeg-Builds/releases/download/${FFMPEG_RELEASE}/${FFMPEG_ASSET}"; \
    printf '%s  %s\n' "$FFMPEG_SHA256" btbn.tar.xz | sha256sum -c -; \
    mkdir -p /opt/ff && tar -xJf btbn.tar.xz -C /opt/ff --strip-components=1; \
    install -m 0755 /opt/ff/bin/ffmpeg /usr/local/bin/ffmpeg; \
    rm -rf /tmp/btbn.tar.xz /opt/ff; \
    ffmpeg -version | head -1; \
    ffmpeg -hide_banner -encoders 2>/dev/null | grep -q libopus || { echo "FATAL: ffmpeg lacks libopus encoder"; exit 1; }

# Non-root app user shares the PulseAudio session with the game.
RUN useradd -m -u 1000 app

# noVNC web client.
RUN set -e; \
    curl -fsSL --retry 3 -o /tmp/novnc.tgz \
      "https://github.com/novnc/noVNC/archive/refs/tags/v${NOVNC_VERSION}.tar.gz"; \
    printf '%s  %s\n' "$NOVNC_SHA256" /tmp/novnc.tgz | sha256sum -c -; \
    tar -xzf /tmp/novnc.tgz -C /opt; \
    ln -s "/opt/noVNC-${NOVNC_VERSION}" /opt/novnc; \
    rm -f /tmp/novnc.tgz

# Prebuilt SDL2 stack + Chocolate Doom (compiled in CI inside this base image).
RUN set -e; \
    curl -fsSL --retry 3 --max-time 240 -o /tmp/prebuilt.tgz \
      "https://github.com/somoore/hellbox/releases/download/${PREBUILT_RELEASE}/${PREBUILT_ASSET}"; \
    printf '%s  %s\n' "$PREBUILT_SHA256" /tmp/prebuilt.tgz | sha256sum -c -; \
    tar -xzf /tmp/prebuilt.tgz -C /; \
    rm -f /tmp/prebuilt.tgz; \
    echo "/usr/local/lib" > /etc/ld.so.conf.d/local.conf && ldconfig; \
    if ldd /usr/local/bin/chocolate-doom | grep -q "not found"; then \
      echo "FATAL: chocolate-doom has unresolved libraries"; \
      ldd /usr/local/bin/chocolate-doom; exit 1; \
    fi; \
    echo "native chocolate-doom at:" && ls -la /usr/local/bin/chocolate-doom

# Stage app WADs, or fetch shareware DOOM1.WAD.
COPY app/ /home/app/app/
RUN set -e; \
    if ! ls /home/app/app/*.wad /home/app/app/*.WAD >/dev/null 2>&1; then \
        echo "[build] fetching shareware DOOM1.WAD"; \
        curl -fsSL --retry 3 -o /home/app/app/DOOM1.WAD \
          "https://raw.githubusercontent.com/nneonneo/universal-doom/${DOOM1_WAD_COMMIT}/DOOM1.WAD"; \
        printf '%s  %s\n' "$DOOM1_WAD_SHA256" /home/app/app/DOOM1.WAD | sha256sum -c -; \
    fi; \
    wad="$(ls /home/app/app/*.wad /home/app/app/*.WAD 2>/dev/null | head -1)"; \
    [ -n "$wad" ] || { echo "FATAL: no IWAD present"; exit 1; }; \
    sz=$(stat -c%s "$wad"); [ "$sz" -ge 4000000 ] || { echo "FATAL: IWAD too small ($sz)"; exit 1; }; \
    head -c 4 "$wad" | grep -q IWAD || { echo "FATAL: $wad is not an IWAD"; exit 1; }; \
    chown -R app:app /home/app/app; \
    echo "== staged app files =="; ls -l /home/app/app

# Browser page.
COPY index.html /opt/novnc/index.html

# Runtime scripts.
COPY rootfs/ /
RUN chmod +x /opt/capsule/start.sh /opt/capsule/run_app.sh

# Documentation only — real exposure is the auth token's allowedPorts (6901-6904).
# The readiness hook (:9000) is loopback-internal and deliberately NOT listed.
EXPOSE 6901 6902 6903 6904
CMD ["/opt/capsule/start.sh"]
