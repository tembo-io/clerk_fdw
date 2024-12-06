PG_CONFIG   ?= $(shell which pg_config)
PGRXV        = $(shell perl -nE '/^pgrx\s+=\s"=?([^"]+)/ && do { say $$1; exit }' Cargo.toml)
PGV          = $(shell perl -E 'shift =~ /(\d+)/ && say $$1' "$(shell $(PG_CONFIG) --version)")
DISTNAME     = $(shell grep -m 1 '^name' Trunk.toml | sed -e 's/[^"]*"\([^"]*\)",\{0,1\}/\1/')
DISTVERSION  = $(shell grep -m 1 '^version' Trunk.toml | sed -e 's/[^"]*"\([^"]*\)",\{0,1\}/\1/')

all: package

.DEFAULT_GOAL: package # Build for the PostgreSQL cluster identified by pg_config.
package:
	@cargo pgrx package --pg-config "$(PG_CONFIG)"

.PHONY: install # Install jsonschema into the PostgreSQL cluster identified by pg_config.
install:
	@cargo pgrx install --release --pg-config "$(PG_CONFIG)"

.PHONY: test # Run the full test suite against the PostgreSQL version identified by pg_config.
test:
	@cargo test --all --no-default-features --features "pg$(PGV) pg_test" -- --nocapture

clean:
	@rm -rf META.json $(DISTNAME)-$(DISTVERSION).zip

.PHONY: pg-version # Print the current PGRX version from Cargo.toml
pgrx-version:
	@echo $(PGRXV)

.PHONY: pg-version # Print the current Postgres version reported by pg_config.
pg-version: Cargo.toml
	@echo $(PGV)

.PHONY: install-pgrx # Install the version of PGRX specified in Cargo.toml.
install-pgrx: Cargo.toml
	@cargo install --locked cargo-pgrx --version "$(PGRXV)"

.PHONY: pgrx-init # Initialize pgrx for the PostgreSQL version identified by pg_config.
pgrx-init: Cargo.toml
	@cargo pgrx init "--pg$(PGV)"="$(PG_CONFIG)"

.PHONY: lint # Format and lint.
lint:
	@cargo fmt --all --check
	@cargo clippy --features "pg$(PGV)" --no-default-features

# Create the PGXN META.json file.
META.json: META.json.in Cargo.toml
	@sed "s/@CARGO_VERSION@/$(DISTVERSION)/g" $< > $@

# Create a PGXN-compatible zip file.
$(DISTNAME)-$(DISTVERSION).zip: META.json
	git archive --format zip --prefix $(DISTNAME)-$(DISTVERSION)/ --add-file $< -o $(DISTNAME)-$(DISTVERSION).zip HEAD

## pgxn-zip: Create a PGXN-compatible zip file.
pgxn-zip: $(DISTNAME)-$(DISTVERSION).zip
