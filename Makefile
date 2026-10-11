PYTHON ?= python3

.PHONY: test lint build up start play logs down reset e2e e2e-smoke e2e-lab

test:
	docker compose run --rm dev cargo test --workspace

lint:
	docker compose run --rm dev sh -c "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings"

build:
	docker compose build pet-a

# Rebuild after code changes.
up:
	docker compose up --build --wait -d pet-a

# Start the existing image (builds only if none exists), so playing never needs the network.
start:
	docker compose up --wait -d pet-a

play: start
	docker compose exec -it -u learner pet-a bramble tui

logs:
	docker compose logs -f pet-a

down:
	docker compose down

# Wipe your pet's bank and progress back to lesson 1.
reset:
	docker compose down
	docker volume rm bramble_pet-a-data

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
