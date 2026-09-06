from pydantic import AwareDatetime, Field, JsonValue

from .models import DomainModel


class GraphDraft(DomainModel):
    graph_id: str = Field(pattern=r"^[A-Za-z][A-Za-z0-9_-]{0,127}$")
    revision: int = Field(ge=1)
    definition: dict[str, JsonValue]
    layout: dict[str, JsonValue] = Field(default_factory=dict)
    updated_at: AwareDatetime
