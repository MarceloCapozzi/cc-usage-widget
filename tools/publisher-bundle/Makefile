# cc-usage publisher bundle: installs, validates and removes the publisher
# that sends this machine's Claude Code sessions to the cc-usage widget.
#
# Run it inside the extracted bundle folder:
#     make install && make verify
#
# GNU make; recipes are POSIX sh (the logic lives in scripts/*.sh).
# Variables (pass on the command line, e.g. make install PREFIX=/opt/x):
#     PREFIX      install location           default: the cc-usage data dir
#     DATA_DIR    where cc-hook writes        $CC_USAGE_DATA_DIR, else
#                 the spool                   $XDG_DATA_HOME/cc-usage (when
#                                             absolute), else
#                                             ~/.local/share/cc-usage
#     CLAUDE_DIR  Claude Code's folder        $CLAUDE_CONFIG_DIR, else ~/.claude
#                 (a custom one is passed to the publisher as CLAUDE_CONFIG_DIR)
#     UNIT_DIR    systemd user unit folder    $XDG_CONFIG_HOME/systemd/user, else
#                                             ~/.config/systemd/user
#     AUTOSTART   systemd or profile          systemd
#     LINGER=1    also run loginctl enable-linger during install
#     PURGE=1     uninstall also removes the creds, the keys and runtime data
#     FORCE=1     install even without ~/.claude
#     VERIFY_WAIT seconds verify waits for a publish   45
#     CONFIRM_ALIAS=<alias>  confirms replacing this machine's identity
#                 (rotate-key, or install over a lost key) without the prompt

SHELL := /bin/sh
.SHELLFLAGS := -ec
.DEFAULT_GOAL := help
.SUFFIXES:
MAKEFLAGS += --no-builtin-rules --no-print-directory

ifneq ($(wildcard config/publish.json),config/publish.json)
$(error run make inside the extracted bundle folder (make -C <folder> <target>): config/publish.json not found in $(CURDIR))
endif

# Extracted with sudo, the files belong to root (the archive stores owner 0)
# and, being 0600, cannot be read by the user who runs make.
ifneq ($(shell test -r config/publish.json && test -r scripts/lib.sh && echo ok),ok)
$(error cannot read the bundle files in $(CURDIR) (config/publish.json belongs to $(shell ls -ld config/publish.json | awk '{print $$3}')). If the archive was extracted with sudo, fix it with:  sudo chown -R "$(shell id -un):" "$(CURDIR)"  (or extract it again without sudo))
endif

# The variables listed above are set on the command line only; one of the
# same name in the environment is ignored. Termux and build tools export
# PREFIX, which must not move the secrets elsewhere, an exported PURGE=1
# must not purge them, and an exported CONFIRM_ALIAS must not replace this
# machine's identity. (CC_USAGE_DATA_DIR and XDG_DATA_HOME are read on
# purpose, below, and so are CLAUDE_CONFIG_DIR and XDG_CONFIG_HOME.)
BUNDLE_VARS := DATA_DIR PREFIX UNIT_DIR PROFILE CLAUDE_DIR AUTOSTART LINGER PURGE FORCE VERIFY_WAIT VERIFY_SKIP_MODES CONFIRM_ALIAS
$(foreach v,$(BUNDLE_VARS),$(if $(filter environment,$(origin $(v))),$(eval undefine $(v))))

# The tooling copy make install leaves in $(PREFIX)/bundle remembers where it
# installed, so verify, status and uninstall run from it act on that install.
ifneq ($(wildcard config/installed-copy),)
INSTALLED_VARS := DATA_DIR PREFIX UNIT_DIR PROFILE CLAUDE_DIR
$(foreach v,$(INSTALLED_VARS),$(eval INSTALLED_$(v) := $$(shell sed -n 's/^$(v)=//p' config/installed-copy | head -n 1)))
$(foreach v,$(INSTALLED_VARS),$(if $(INSTALLED_$(v)),$(eval $(v) ?= $$(INSTALLED_$(v)))))
endif

ifneq ($(CC_USAGE_DATA_DIR),)
DATA_DIR ?= $(CC_USAGE_DATA_DIR)
else ifneq ($(filter /%,$(firstword $(XDG_DATA_HOME))),)
DATA_DIR ?= $(XDG_DATA_HOME)/cc-usage
else
DATA_DIR ?= $(HOME)/.local/share/cc-usage
endif
PREFIX ?= $(DATA_DIR)
ifneq ($(XDG_CONFIG_HOME),)
UNIT_DIR ?= $(XDG_CONFIG_HOME)/systemd/user
else
UNIT_DIR ?= $(HOME)/.config/systemd/user
endif
PROFILE ?= $(HOME)/.profile
ifneq ($(CLAUDE_CONFIG_DIR),)
CLAUDE_DIR ?= $(CLAUDE_CONFIG_DIR)
else
CLAUDE_DIR ?= $(HOME)/.claude
endif
AUTOSTART ?= systemd
LINGER ?= 0
PURGE ?= 0
FORCE ?= 0
VERIFY_WAIT ?= 45
VERIFY_SKIP_MODES ?= 0
CONFIRM_ALIAS ?=

# The machine alias (cc-publish's config) and NATS user (bundle.env).
ALIAS := $(shell sed -n 's/^[[:space:]]*"alias"[[:space:]]*:[[:space:]]*"\([a-z0-9-]*\)".*$$/\1/p' config/publish.json | head -n 1)
NATS_USER := $(shell sed -n 's/^NATS_USER=//p' config/bundle.env 2>/dev/null | head -n 1)
ifeq ($(NATS_USER),)
NATS_USER := cc-sessions.pub-$(ALIAS)
endif
ifeq ($(ALIAS),)
$(error config/publish.json has no valid "alias")
endif

export DATA_DIR PREFIX UNIT_DIR PROFILE CLAUDE_DIR AUTOSTART LINGER PURGE FORCE VERIFY_WAIT VERIFY_SKIP_MODES CONFIRM_ALIAS

.PHONY: help check-prereqs install setup verify status logs restart hooks rotate-key uninstall autostart-profile

help:
	@echo "cc-usage publisher bundle for '$(ALIAS)' ($(NATS_USER))"
	@echo ""
	@echo "  make check-prereqs      check this machine (Linux, architecture, systemd, Claude Code, checksums)"
	@echo "  make install            install into $(PREFIX), register hooks, start the systemd user service"
	@echo "  make verify             validate everything and print a PASS/FAIL table (exit 1 on any FAIL)"
	@echo "  make setup              check-prereqs, install and verify in one go"
	@echo "  make status             service, publisher and hook status"
	@echo "  make logs               follow the publisher's log"
	@echo "  make restart            restart the publisher"
	@echo "  make hooks              register the hooks and the status line chain again (idempotent)"
	@echo "  make rotate-key         replace this machine's identity (its key pair); asks for the alias"
	@echo "  make uninstall          remove everything except the secrets (PURGE=1 removes them too)"
	@echo "  make autostart-profile  install with a ~/.profile autostart instead of systemd (WSL without systemd)"
	@echo ""
	@echo "  First time:  make install && make verify"
	@echo "  The bundle folder holds secrets: delete it once make verify passes."

check-prereqs:
	@sh ./scripts/check-prereqs.sh

install: check-prereqs
	@sh ./scripts/install.sh

setup: install
	@sh ./scripts/verify.sh

verify:
	@sh ./scripts/verify.sh

status:
	@sh ./scripts/ctl.sh status

logs:
	@sh ./scripts/ctl.sh logs

restart:
	@sh ./scripts/ctl.sh restart

hooks:
	@sh ./scripts/ctl.sh hooks

rotate-key:
	@sh ./scripts/rotate-key.sh

uninstall:
	@sh ./scripts/uninstall.sh

autostart-profile:
	@$(MAKE) install AUTOSTART=profile
