"""Database connection utilities for the training pipeline."""

import os
from sqlalchemy import create_engine

DATABASE_URL = os.getenv(
    "DATABASE_URL",
    "postgresql://cstat:cstat@localhost:5432/cstat",
)

# SQLAlchemy 1.4+ rejects the bare `postgres://` scheme; the rest of the
# repo (Rust sqlx, the Heroku-style .env.example) uses it freely.
if DATABASE_URL.startswith("postgres://"):
    DATABASE_URL = "postgresql://" + DATABASE_URL[len("postgres://"):]


def get_engine():
    return create_engine(DATABASE_URL)
