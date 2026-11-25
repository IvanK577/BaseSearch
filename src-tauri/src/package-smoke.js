async (directory) => {
  const invoke = (name, args) => window.__TAURI_INTERNALS__.invoke(name, args);
  const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const path = (file) => directory + (directory.includes("\\") ? "\\" : "/") + file;
  const button = (label, scope = document) =>
    [...scope.querySelectorAll("button")].find((node) => node.textContent.trim() === label);
  const stage = "base-search-package-fixture-loaded";

  function ensure(condition, message) {
    if (!condition) throw new Error(message);
  }

  async function waitFor(label, predicate) {
    for (let attempt = 0; attempt < 100; attempt++) {
      const ready = predicate();
      if (ready) return ready;
      await pause(125);
    }
    throw new Error("Could not verify " + label);
  }

  async function finished(id) {
    for (let attempt = 0; attempt < 180; attempt++) {
      const job = await invoke("job_status", { id });
      if (job.state === "complete") return job.result;
      ensure(job.state === "running", job.message);
      await pause(250);
    }
    throw new Error("Desktop job timed out");
  }

  try {
    if (!sessionStorage.getItem(stage)) {
      await waitFor("welcome actions", () => button("Добавить файлы") && button("Перенести базу 2.2.0"));
      const source = path("smoke.csv");
      const preview = await invoke("preview_file", { path: source });
      ensure(preview.sheets.length === 1 && preview.sheets[0].rows.length >= 2, "CSV preview is incomplete");
      const loaded = await finished(await invoke("start_import", {
        files: [{
          path: source,
          sheets: [{
            key: preview.sheets[0].key,
            headers: preview.sheets[0].headers,
            roles: ["date", "company", "product", null, "value", "currency", "weight", "weightUnit"],
            fixedCurrency: null,
            fixedWeightUnit: null,
          }],
        }],
      }));
      ensure(loaded.rows === 2, "Import changed fixture row count");
      sessionStorage.setItem(stage, "yes");
      location.reload();
      return;
    }

    await waitFor("initial search results", () => {
      const rows = document.querySelector("table.records tbody");
      return rows?.textContent.includes("Coffee shipment") && rows.textContent.includes("Tea shipment");
    });
    const input = document.querySelector("input[aria-label='Поисковый запрос']");
    ensure(input?.form, "Search field or form is missing");
    input.value = "coffee";
    input.form.requestSubmit();
    await waitFor("filtered search rows", () => {
      const rows = document.querySelector("table.records tbody");
      return rows?.textContent.includes("Coffee shipment") && !rows.textContent.includes("Tea shipment");
    });

    document.querySelector("button[aria-label='Открыть строку 1']").click();
    await waitFor("record values", () =>
      document.querySelector("dialog[open] .detail-fields.raw")?.textContent.includes("001122"));
    document.querySelector("dialog[open] button[aria-label='Закрыть']").click();

    document.querySelector("nav button[aria-label='Сводка']").click();
    await waitFor("query-scoped summary", () => {
      const main = document.querySelector("main");
      return main.querySelector("h1")?.textContent === "Сводка"
        && main.textContent.includes("USD") && !main.textContent.includes("EUR");
    });
    document.querySelector("nav button[aria-label='Поиск']").click();
    const exportButton = await waitFor("export action", () => {
      const candidate = button("Выгрузить");
      return candidate && !candidate.disabled && candidate;
    });
    exportButton.click();
    await waitFor("export modal", () => {
      const dialog = document.querySelector("dialog[open]");
      return dialog?.textContent.includes("Строк по применённому запросу")
        && dialog.querySelector("p b")?.textContent.trim() === "1";
    });

    const info = await invoke("workspace_info");
    ensure(info.rowCount === 2, "Workspace row count is incorrect");
    const query = { text: "coffee", filters: [], sort: null, revision: info.revision };
    const exported = await finished(await invoke("start_export", {
      query, path: path("coffee.csv"), columns: null,
    }));
    ensure(exported.rows === 1, "Export did not preserve the active query");
    await invoke("package_smoke_result", { error: null });
  } catch (error) {
    await invoke("package_smoke_result", { error: String(error) });
  }
}
