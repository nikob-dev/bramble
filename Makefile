PYTHON ?= python3

.PHONY: test lint build up play logs down e2e e2e-smoke e2e-lab

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

# Isolated project + port so the e2e tests never touch your pet's data.
# Each script gets a fresh pet: the lab checks need lesson 1 from the start.
E2E = BRAMBLE_PET_A_PORT=18081 docker compose -p bramble-e2e
e2e: e2e-lab e2e-smoke

e2e-smoke:
	$(E2E) up --build --wait -d pet-a
	$(PYTHON) tests/e2e/tui_smoke.py bramble-e2e http://localhost:18081; \
	status=$$?; $(E2E) down -v; exit $$status

e2e-lab:
	$(E2E) up --build --wait -d pet-a
	$(PYTHON) tests/e2e/lab_checks.py bramble-e2e http://localhost:18081; \
	status=$$?; $(E2E) down -v; exit $$status
