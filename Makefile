# A command index, not a build graph. See the spec: the real dependency edges
# differ between a local run and CI, so they live in the scripts, once.
.PHONY: help preflight release verify deb rpm test-scripts

help:
	@echo 'make preflight              run every pre-tag check'
	@echo 'make release VERSION=0.5.2  cut a release end to end'
	@echo 'make verify VERSION=0.5.2   read back every channel'
	@echo 'make deb BIN=<path> VERSION=<v> ARCH=<amd64|arm64>'
	@echo 'make rpm BIN=<path> VERSION=<v> ARCH=<x86_64|aarch64>'
	@echo 'make test-scripts           shellcheck + package tests'

preflight:
	@scripts/preflight.sh

release:
	@test -n '$(VERSION)' || { echo 'VERSION= is required'; exit 1; }
	@scripts/release.sh '$(VERSION)'

verify:
	@test -n '$(VERSION)' || { echo 'VERSION= is required'; exit 1; }
	@scripts/verify-release.sh '$(VERSION)'

deb:
	@scripts/package-deb.sh '$(BIN)' '$(VERSION)' '$(ARCH)'

rpm:
	@scripts/package-rpm.sh '$(BIN)' '$(VERSION)' '$(ARCH)'

test-scripts:
	@shellcheck scripts/*.sh scripts/test/*.sh
	@scripts/test/packages.sh
