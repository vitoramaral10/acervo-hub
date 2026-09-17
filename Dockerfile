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

# O `target/` e o registry ficam em cache mount: não viram camada, então o
# build de CI reaproveita a compilação sem carregar nada disso na imagem. Como
# o cache mount some ao fim do RUN, o binário é copiado para fora aqui dentro.
RUN --mount=type=cache,target=/src/target,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --release --locked --target "${TARGET}" --bin acervo-hub \
 && install -D "/src/target/${TARGET}/release/acervo-hub" /out/acervo-hub

# Imagem final: só o binário. Sem shell, sem gerenciador de pacotes, sem curl.
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab AS runtime

# Root é dono e o processo não pode reescrever o próprio binário: escrita
# arbitrária deixa de virar execução de código persistente.
COPY --from=builder --chown=root:root --chmod=0755 /out/acervo-hub /usr/local/bin/acervo-hub

# UID numérico: `runAsNonRoot` não resolve nome de usuário.
USER 65532:65532

# Forma exec — na forma shell o PID 1 viraria `/bin/sh -c`, que engole SIGTERM
# (e a distroless nem tem shell).
ENTRYPOINT ["/usr/local/bin/acervo-hub"]

# Padrão seguro: simular. Executar de verdade exige dizer `apply`.
CMD ["plan"]
