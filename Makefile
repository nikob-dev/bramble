PYTHON ?= python3

.PHONY: test lint build up start play network peers logs down reset e2e e2e-smoke e2e-lab e2e-net clean-docker

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

# All four nodes: your bank, two counterparty banks, and the operator (decision 015).
NODES = pet-a pet-b pet-c operator
network:
	docker compose up --build --wait -d $(NODES)

# Which peers each node has discovered (the peer port is only reachable inside compose).
peers:
	@for node in $(NODES); do \
		printf '%-9s ' $$node; \
		docker compose exec -T $$node curl -fsS localhost:9090/net/peers || echo "(not running)"; \
		echo; \
	done

logs:
	docker compose logs -f pet-a

down:
	docker compose down

# Reclaim disk from Docker: build cache and dangling images (all regenerable).
# Keeps volumes, so your pet and the cargo cache survive. Next build is slower.
clean-docker:
	docker builder prune -af
	docker image prune -f
	docker system df

# Wipe every node's bank and progress (your pet back to lesson 1). Keeps the cargo cache.
reset:
	docker compose down
	docker volume rm $$(docker volume ls -q --filter name=bramble_pet- --filter name=bramble_operator-)

# Isolated project + port so the e2e tests never touch your pet's data.
# Each script gets a fresh pet: the lab checks need lesson 1 from the start.
E2E = BRAMBLE_PET_A_PORT=18081 docker compose -p bramble-e2e
e2e: e2e-lab e2e-smoke e2e-net

e2e-smoke:
	$(E2E) up --build --wait -d pet-a
	$(PYTHON) tests/e2e/tui_smoke.py bramble-e2e http://localhost:18081; \
	status=$$?; $(E2E) down -v; exit $$status

# The whole network (3 banks + operator) on its own ports: RTP, ACH cutoff, reconciliation.
E2E_NET = BRAMBLE_PET_A_PORT=18281 BRAMBLE_PET_B_PORT=18282 BRAMBLE_PET_C_PORT=18283 \
	BRAMBLE_OPERATOR_PORT=18290 docker compose -p bramble-e2e-net
e2e-net:
	$(E2E_NET) up --build --wait -d $(NODES)
	$(PYTHON) tests/e2e/network_smoke.py bramble-e2e-net; \
	status=$$?; $(E2E_NET) down -v; exit $$status

e2e-lab:
	$(E2E) up --build --wait -d pet-a
	$(PYTHON) tests/e2e/lab_checks.py bramble-e2e http://localhost:18081; \
	status=$$?; $(E2E) down -v; exit $$status
