import { describe, expect, it } from "vitest";
import { defaultColorField } from "./data";
import type { FieldInfo } from "./types";

const f = (id: string, kind: "categorical" | "numeric", group = "annotation") => ({ id, kind, group, label: id }) as FieldInfo;

describe("default colouring", () => {
  it("prefers the major cell type, then a fine cell type, then the first categorical annotation", () => {
    const xenium = [f("segmentation_method", "categorical", "cells"), f("cell_area", "numeric", "cells")];
    expect(defaultColorField([...xenium, f("final_cell_type", "categorical"), f("major_cell_type", "categorical")])?.id).toBe("major_cell_type");
    expect(defaultColorField([...xenium, f("final_cell_type", "categorical"), f("fine_cell_type", "categorical")])?.id).toBe("fine_cell_type");
    expect(defaultColorField([...xenium, f("score", "numeric"), f("final_cell_type", "categorical")])?.id).toBe("final_cell_type");
    expect(defaultColorField(xenium)).toBeUndefined();
  });
});
