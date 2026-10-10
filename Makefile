.PHONY: test lint build up play down

test:
	docker compose run --rm dev cargo test --workspace

lint:
	docker compose run --rm dev sh -c "cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings"

build:
	docker compose build pet-a

up:
	docker compose up --build -d pet-a

play: up
	docker compose exec -it pet-a bramble tui

down:
	docker compose down
