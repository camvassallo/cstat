"""Database connection utilities for the training pipeline."""

import os
from sqlalchemy import create_engine

DATABASE_URL = os.getenv(
    "DATABASE_URL",
    "postgresql://cstat:cstat@localhost:5432/cstat",
)


def get_engine():
    return create_engine(DATABASE_URL)
