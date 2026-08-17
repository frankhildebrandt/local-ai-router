import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import App from "./App";

describe("Local AI Router shell", () => {
  it("shows the private gateway quickstart without requiring a backend", async () => {
    render(<App />);
    expect(await screen.findByText("Your models, one local endpoint.")).toBeTruthy();
    expect(screen.getAllByText("http://127.0.0.1:11435/v1").length).toBeGreaterThan(0);
  });
});
