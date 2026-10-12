BIN := target/release/fastscancleaner
PID := target/fastscancleaner.pid
LOG := target/fastscancleaner.log
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

.PHONY: run start stop restart status logs build test clean release

run: build ## Run in the foreground (Ctrl+C to quit)
	$(BIN)

start: build ## Run in the background
	@if [ -f $(PID) ] && kill -0 $$(cat $(PID)) 2>/dev/null; then echo "already running (pid $$(cat $(PID)))"; \
	else $(BIN) > $(LOG) 2>&1 & echo $$! > $(PID); echo "started (pid $$(cat $(PID))), log: $(LOG)"; fi

stop: ## Stop the background app
	@if [ -f $(PID) ] && kill $$(cat $(PID)) 2>/dev/null; then echo "stopped"; else echo "not running"; fi; rm -f $(PID)

restart: stop start

status:
	@if [ -f $(PID) ] && kill -0 $$(cat $(PID)) 2>/dev/null; then echo "running (pid $$(cat $(PID)))"; else echo "not running"; fi

logs:
	@tail -f $(LOG)

build:
	cargo build --release

test:
	cargo test

clean: stop
	cargo clean

release: test ## Tag the Cargo.toml version and push it; CI builds the installers
	@git diff --quiet && git diff --cached --quiet || { echo "commit your changes first"; exit 1; }
	@git rev-parse -q --verify refs/tags/v$(VERSION) >/dev/null && { echo "v$(VERSION) already tagged, bump version in Cargo.toml"; exit 1; } || true
	git tag v$(VERSION)
	git push origin v$(VERSION)
