SIGN_IDENTITY ?= -

.PHONY: run verify check

run:
	cd arcbox-desktop && ARCBOX_USE_LOCAL_RUNTIME=1 SIGN_IDENTITY='$(SIGN_IDENTITY)' ./script/build_and_run.sh

verify:
	cd arcbox-desktop && ARCBOX_USE_LOCAL_RUNTIME=1 SIGN_IDENTITY='$(SIGN_IDENTITY)' ./script/build_and_run.sh --verify

check:
	$(MAKE) -C arcbox-desktop test-xtask lint-xtask
