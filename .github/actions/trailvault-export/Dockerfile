FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends software-properties-common ca-certificates \
    && add-apt-repository --yes ppa:git-core/ppa \
    && apt-get update && apt-get install -y --no-install-recommends git curl zip \
    && rm -rf /var/lib/apt/lists
RUN git init --bare --initial-branch main /tmp/trailvault-git-probe \
    && git --git-dir=/tmp/trailvault-git-probe remote add origin https://old.invalid/ \
    && test "$(git --git-dir=/tmp/trailvault-git-probe \
        -c remote.origin.url= -c remote.origin.url=https://collector.invalid/ \
        remote get-url --all origin)" = "https://collector.invalid/"
COPY trailvault /usr/local/bin/trailvault
COPY register.sh export.sh /
RUN chmod 755 /register.sh /export.sh /usr/local/bin/trailvault
