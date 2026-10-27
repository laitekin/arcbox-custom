SIGN_IDENTITY ?= -

.PHONY: run run-fast run-ui build-fast build-ui verify check

run:
	cd arcbox-desktop && ARCBOX_USE_LOCAL_RUNTIME=1 SIGN_IDENTITY='$(SIGN_IDENTITY)' ./script/build_and_run.sh

run-fast:
	cd arcbox-desktop && ARCBOX_USE_LOCAL_RUNTIME=1 SIGN_IDENTITY='$(SIGN_IDENTITY)' ./script/build_and_run.sh --fast

run-ui:
	cd arcbox-desktop && SIGN_IDENTITY='$(SIGN_IDENTITY)' ./script/build_and_run.sh --ui-only

build-fast:
	$(MAKE) -C arcbox-desktop build-fast ARCBOX_USE_LOCAL_RUNTIME=1 ARCBOX_HOST_TARGET_DIR='$(CURDIR)/arcbox/target' SIGN_IDENTITY='$(SIGN_IDENTITY)'

build-ui:
	$(MAKE) -C arcbox-desktop build-ui SIGN_IDENTITY='$(SIGN_IDENTITY)'

verify:
	cd arcbox-desktop && ARCBOX_USE_LOCAL_RUNTIME=1 SIGN_IDENTITY='$(SIGN_IDENTITY)' ./script/build_and_run.sh --verify

check:
	python3 arcbox-desktop/script/test_build_modes.py
	$(MAKE) -C arcbox-desktop test-xtask lint-xtask
