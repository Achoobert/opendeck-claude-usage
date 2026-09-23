PLUGIN_DIR := com.achoobert.claudeusage.sdPlugin
BUNDLE_ID := $(basename $(PLUGIN_DIR))
PACKAGE := oaclaude-usage

UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)
# Rust target triples use "aarch64", not Darwin/Linux's "arm64".
ARCH := $(patsubst arm64,aarch64,$(UNAME_M))

ifeq ($(UNAME_S),Darwin)
	TRIPLE := $(ARCH)-apple-darwin
	OPENDECK_CONFIG := $(HOME)/Library/Application Support/OpenDeck
else
	TRIPLE := $(ARCH)-unknown-linux-gnu
	OPENDECK_CONFIG := $(or $(XDG_CONFIG_HOME),$(HOME)/.config)/opendeck
endif

BINARY := $(PACKAGE)-$(TRIPLE)

.PHONY: all build stage package install clean

all: build

build:
	cargo build --release
	mkdir -p stage
	cp target/release/$(PACKAGE) stage/$(BINARY)

stage: build
	mkdir -p $(PLUGIN_DIR)/actions
	cp target/release/$(PACKAGE) $(PLUGIN_DIR)/$(BINARY)
	cp assets/manifest.json $(PLUGIN_DIR)/manifest.json
	cp assets/pi.html $(PLUGIN_DIR)/pi.html
	cp assets/icon.svg $(PLUGIN_DIR)/icon.svg
	cp assets/category-icon.svg $(PLUGIN_DIR)/category-icon.svg
	cp assets/actions/icon.svg $(PLUGIN_DIR)/actions/icon.svg

package: stage
	rm -f $(BUNDLE_ID).zip
	zip -rq $(BUNDLE_ID).zip $(PLUGIN_DIR)

install: stage
	mkdir -p "$(OPENDECK_CONFIG)/plugins"
	cp -r $(PLUGIN_DIR) "$(OPENDECK_CONFIG)/plugins/"

clean:
	cargo clean
	rm -rf stage $(PLUGIN_DIR) $(BUNDLE_ID).zip
