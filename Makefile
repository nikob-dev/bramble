PYTHON ?= python3

.PHONY: test lint build up play logs down e2e

test:
	docker compose run --rm dev cargo test --workspace

lint:
	docker compose run --rm dev sh -c "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings"

build:
	docker compose build pet-a

up:
	docker compose up --build --wait -d pet-a

play: up
	docker compose exec -it -u learner pet-a bramble tui

logs:
	docker compose logs -f pet-a

down:
	docker compose down

# Isolated project + port so the smoke test never touches your pet's data.
e2e:
	BRAMBLE_PET_A_PORT=18081 docker compose -p bramble-e2e up --build --wait -d pet-a
	$(PYTHON) tests/e2e/tui_smoke.py bramble-e2e http://localhost:18081; \
	status=$$?; docker compose -p bramble-e2e down -v; exit $$status
