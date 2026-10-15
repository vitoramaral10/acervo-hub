# syntax=docker/dockerfile:1

# Estágio descartável: toolchain, musl e o registry do cargo ficam aqui e não
# chegam à imagem final.
FROM rust:1.98-bookworm@sha256:9a73a5088750b4c95158ab26629c854c3d6fc4b173cb7bc8079ad252d8ed7bfa AS builder

# Alvo musl para produzir binário estático. Só é possível porque o TLS vem do
# rustls, em Rust puro — com OpenSSL, a ligação estática seria dor.
ARG TARGET=x86_64-unknown-linux-musl
RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add "${TARGET}"

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# Inclui o bundle versionado em crates/acervo-hub/src/api/ui/dist.

# O `target/` e o registry ficam em cache mount: não viram camada, então o
# build de CI reaproveita a compilação sem carregar nada disso na imagem. Como
# o cache mount some ao fim do RUN, o binário é copiado para fora aqui dentro.
RUN --mount=type=cache,target=/src/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --profile dist --locked --target "${TARGET}" --bin acervo-hub \
 && install -D "/src/target/${TARGET}/dist/acervo-hub" /out/acervo-hub

# `ffprobe` e `ffmpeg` estáticos: o primeiro lê as faixas de áudio e legenda do
# que o acervo importa (o app de legendas depende disso); o segundo tira os
# quadros das capturas de "Para lançar". Binários únicos, sem bibliotecas; a
# imagem fixada por digest traz os dois na raiz (`/ffmpeg` e `/ffprobe`,
# conferido listando o conteúdo dela).
FROM mwader/static-ffmpeg:7.1.1@sha256:11a44711684c0b9f754c047dcd64235b8b52deab251bd0e0a86f22faa160749c AS ffmpeg

# MediaInfo CLI estático: o formulário do Amigos exige o relatório do MediaInfo
# de verdade, e a imagem final (distroless) não tem libc, então o binário sai
# daqui ligado estaticamente em musl. Compilado do pacote oficial da MediaArea,
# com o sha256 do tarball conferido; ZenLib e MediaInfoLib vêm no pacote, a zlib
# é a estática do Alpine (o pacote tentaria baixá-la por git), e libcurl/libmms
# ficam de fora — o CLI só lê arquivo local.
FROM alpine:3.22@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8 AS mediainfo
# O SBOM do BuildKit só olha o estágio final; sem isto o scan não vê a zlib, a
# ZenLib nem a MediaInfoLib embutidas no binário estático.
ARG BUILDKIT_SBOM_SCAN_STAGE=true
ARG MEDIAINFO_VERSION=26.10
ARG MEDIAINFO_SHA256=7785bb75d69efd21b7c0b2c2269164aeb21cea2be6f6b9103ad79743af6fdec7
RUN apk add --no-cache build-base file xz curl zlib-dev zlib-static
WORKDIR /build
RUN curl -fsSLO "https://mediaarea.net/download/binary/mediainfo/${MEDIAINFO_VERSION}/MediaInfo_CLI_${MEDIAINFO_VERSION}_GNU_FromSource.tar.xz" \
 && echo "${MEDIAINFO_SHA256}  MediaInfo_CLI_${MEDIAINFO_VERSION}_GNU_FromSource.tar.xz" | sha256sum -c - \
 && tar -xf "MediaInfo_CLI_${MEDIAINFO_VERSION}_GNU_FromSource.tar.xz" --strip-components=1 \
 && rm "MediaInfo_CLI_${MEDIAINFO_VERSION}_GNU_FromSource.tar.xz"
# `LDFLAGS=-static` vale para ZenLib e MediaInfoLib; o libtool do CLI engole o
# `-static` e liga dinâmico, então o executável é religado com `-all-static`.
RUN LDFLAGS="-static" ./CLI_Compile.sh --disable-shared --enable-static --without-libcurl --without-libmms \
 && rm MediaInfo/Project/GNU/CLI/mediainfo \
 && make -C MediaInfo/Project/GNU/CLI LDFLAGS="-static -all-static" \
 && install -D MediaInfo/Project/GNU/CLI/mediainfo /out/mediainfo \
 && strip /out/mediainfo
# Falha o estágio se o binário não for estático ou não rodar.
RUN file /out/mediainfo | grep -q 'statically linked' \
 && ! ldd /out/mediainfo >/dev/null 2>&1 \
 && /out/mediainfo --Version

# Imagem final: o binário, o `ffprobe`, o `ffmpeg` e o `mediainfo`. Sem shell,
# sem gerenciador de pacotes, sem curl.
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab AS runtime

# Root é dono e o processo não pode reescrever o próprio binário: escrita
# arbitrária deixa de virar execução de código persistente.
COPY --from=builder --chown=root:root --chmod=0755 /out/acervo-hub /usr/local/bin/acervo-hub
COPY --from=ffmpeg --chown=root:root --chmod=0755 /ffprobe /usr/local/bin/ffprobe
COPY --from=ffmpeg --chown=root:root --chmod=0755 /ffmpeg /usr/local/bin/ffmpeg
# Estático (estágio `mediainfo`); `ACERVO_MEDIAINFO` aponta para outro binário.
COPY --from=mediainfo --chown=root:root --chmod=0755 /out/mediainfo /usr/local/bin/mediainfo

# UID numérico: `runAsNonRoot` não resolve nome de usuário.
USER 65532:65532

# Forma exec — na forma shell o PID 1 viraria `/bin/sh -c`, que engole SIGTERM
# (e a distroless nem tem shell).
ENTRYPOINT ["/usr/local/bin/acervo-hub"]

# O serviço: indexadores, interface e tarefas de fundo, a limpeza entre elas.
CMD ["serve"]
