# LambdaDoom capsule: native aarch64 Chocolate Doom streamed to the browser.
FROM public.ecr.aws/lambda/microvms:al2023-minimal

ARG FFMPEG_RELEASE=autobuild-2026-06-26-13-36
ARG FFMPEG_ASSET=ffmpeg-n8.1.2-linuxarm64-gpl-8.1.tar.xz
ARG FFMPEG_SHA256=42be928da25b3e1f7dc271b78a0087dbfcf4738df1d7bf46d158a86eedb285da
ARG NOVNC_VERSION=1.5.0
ARG NOVNC_SHA256=6a73e41f98388a5348b7902f54b02d177cb73b7e5eb0a7a0dcf688cc2c79b42a
ARG SDL2_VERSION=2.30.9
ARG SDL2_SHA256=24b574f71c87a763f50704bbb630cbe38298d544a1f890f099a4696b1d6beba4
ARG SDL2_MIXER_VERSION=2.8.0
ARG SDL2_MIXER_SHA256=1cfb34c87b26dbdbc7afd68c4f545c0116ab5f90bbfecc5aebe2a9cb4bb31549
ARG SDL2_NET_VERSION=2.2.0
ARG SDL2_NET_SHA256=4e4a891988316271974ff4e9585ed1ef729a123d22c08bd473129179dc857feb
ARG CHOCOLATE_DOOM_VERSION=3.0.1
ARG CHOCOLATE_DOOM_SHA256=a54383beef6a52babc5b00d58fcf53a454f012ced7b1936ba359b13f1f10ac66
ARG DOOM1_WAD_COMMIT=fcf22d9773c62835724f2d25c40933ef051f6600
ARG DOOM1_WAD_SHA256=1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771

# Display, audio, build/runtime deps.
RUN dnf install -y tigervnc-server xterm xsetroot python3.11 python3.11-pip tar gzip xz binutils which procps-ng \
        shadow-utils pulseaudio pulseaudio-utils \
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

# Build SDL2 + Chocolate Doom from source.
RUN dnf install -y gcc make autoconf automake libtool pkgconfig \
        libX11-devel libXext-devel pulseaudio-libs-devel alsa-lib-devel \
    && dnf clean all
RUN for p in libXcursor-devel libXi-devel libXrandr-devel libXfixes-devel \
        libXScrnSaver-devel libxkbcommon-devel; do \
      dnf install -y "$p" || echo "[build] optional dep $p unavailable, skipping"; \
    done; dnf clean all || true
ENV PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:/usr/lib64/pkgconfig

# SDL2: X11 video, PulseAudio/ALSA audio, no GL/Wayland/Vulkan.
WORKDIR /tmp/sdl
RUN curl -fsSL --retry 3 -o sdl.tgz "https://github.com/libsdl-org/SDL/releases/download/release-${SDL2_VERSION}/SDL2-${SDL2_VERSION}.tar.gz" \
    && printf '%s  %s\n' "$SDL2_SHA256" sdl.tgz | sha256sum -c - \
    && tar -xzf sdl.tgz --strip-components=1 \
    && ./configure --disable-video-wayland --disable-video-opengl \
        --disable-video-opengles --disable-video-vulkan \
        --enable-video-x11 --enable-pulseaudio --enable-alsa --disable-sdltest \
    && make -j"$(nproc)" && make install \
    && echo "/usr/local/lib" > /etc/ld.so.conf.d/local.conf && ldconfig \
    && rm -rf /tmp/sdl
WORKDIR /

# SDL2_mixer and SDL2_net against source SDL2.
WORKDIR /tmp/mixer
RUN curl -fsSL --retry 3 -o mixer.tgz "https://github.com/libsdl-org/SDL_mixer/releases/download/release-${SDL2_MIXER_VERSION}/SDL2_mixer-${SDL2_MIXER_VERSION}.tar.gz" \
    && printf '%s  %s\n' "$SDL2_MIXER_SHA256" mixer.tgz | sha256sum -c - \
    && tar -xzf mixer.tgz --strip-components=1 \
    && ./configure --disable-music-mod-mikmod --disable-music-midi-fluidsynth \
        --disable-music-ogg --disable-music-flac --disable-music-mp3-mpg123 \
        --disable-music-opus --disable-sdltest \
    && make -j"$(nproc)" && make install && ldconfig && rm -rf /tmp/mixer
WORKDIR /tmp/net
RUN curl -fsSL --retry 3 -o net.tgz "https://github.com/libsdl-org/SDL_net/releases/download/release-${SDL2_NET_VERSION}/SDL2_net-${SDL2_NET_VERSION}.tar.gz" \
    && printf '%s  %s\n' "$SDL2_NET_SHA256" net.tgz | sha256sum -c - \
    && tar -xzf net.tgz --strip-components=1 \
    && ./configure --disable-sdltest && make -j"$(nproc)" && make install && ldconfig \
    && rm -rf /tmp/net
WORKDIR /

# Chocolate Doom.
WORKDIR /tmp/cdoom
RUN curl -fsSL --retry 3 -o cdoom.tgz "https://github.com/chocolate-doom/chocolate-doom/archive/refs/tags/chocolate-doom-${CHOCOLATE_DOOM_VERSION}.tar.gz" \
    && printf '%s  %s\n' "$CHOCOLATE_DOOM_SHA256" cdoom.tgz | sha256sum -c - \
    && tar -xzf cdoom.tgz --strip-components=1 \
    && ./autogen.sh \
    && ./configure --disable-sdltest CFLAGS="-O2 -fcommon" \
    && make -j"$(nproc)" \
    && install -m 0755 src/chocolate-doom /usr/local/bin/chocolate-doom \
    && echo "native chocolate-doom at:" && ls -la /usr/local/bin/chocolate-doom \
    && rm -rf /tmp/cdoom
WORKDIR /

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

EXPOSE 6901 6902 9000
CMD ["/opt/capsule/start.sh"]
