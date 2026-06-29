# Rust development image with Claude Code baked in.
FROM rust:1-bookworm

# Base tooling.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        pkg-config \
        libssl-dev \
        ripgrep \
        sudo \
    && rm -rf /var/lib/apt/lists/*

# Common Rust components and dev tooling.
RUN rustup component add clippy rustfmt rust-src \
    && cargo install cargo-watch cargo-edit

# Node.js (for the Turbo monorepo frontend) + Yarn via Corepack.
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/* \
    && corepack enable

# Non-root user so files created in mounted volumes stay owned by you.
ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=1000
RUN groupadd --gid ${USER_GID} ${USERNAME} \
    && useradd --uid ${USER_UID} --gid ${USER_GID} -m -s /bin/bash ${USERNAME} \
    && echo "${USERNAME} ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/${USERNAME} \
    && chmod 0440 /etc/sudoers.d/${USERNAME} \
    # Pre-create ~/.claude so a mounted named volume inherits dev ownership
    # (Docker creates fresh volumes as root otherwise, which breaks login).
    && install -d -o ${USERNAME} -g ${USERNAME} /home/${USERNAME}/.claude

USER ${USERNAME}
WORKDIR /workspace

# Claude CLI via the official native installer (installs to ~/.local/bin).
ENV PATH=/home/${USERNAME}/.local/bin:${PATH}
RUN curl -fsSL https://claude.ai/install.sh | bash

# Provide your key at runtime, e.g.:
#   docker run -it -e ANTHROPIC_API_KEY=sk-ant-... -v "$PWD":/workspace <image>
# Or run `claude` and authenticate interactively.

# Add some rustup deps
RUN rustup component add clippy cargo-watch rustfmt rust-src

CMD ["bash"]
