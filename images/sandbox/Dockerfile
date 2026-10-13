# One image per product language. BASE is that language's toolchain image,
# for example rust:<version>-bookworm. Claude Code is installed only when
# CLAUDE_CODE_VERSION is set; EXTRA_PACKAGES is for test images.
ARG BASE=debian:bookworm-slim
FROM ${BASE}
ARG CLAUDE_CODE_VERSION=""
ARG EXTRA_PACKAGES=""
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates util-linux procps ${EXTRA_PACKAGES} \
 && if [ -n "$CLAUDE_CODE_VERSION" ]; then \
      apt-get install -y --no-install-recommends nodejs npm \
      && npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"; \
    fi \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 agent
COPY --chmod=0755 entrypoint.sh /usr/local/bin/kod-entrypoint
ENTRYPOINT ["/usr/local/bin/kod-entrypoint"]
