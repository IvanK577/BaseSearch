import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { Backend } from "./model";

export const desktop: Backend = {
  workspace: () => invoke("workspace_info"),
  chooseFiles: async () =>
    (await open({
      multiple: true,
      title: "Добавить файлы",
      filters: [
        {
          name: "Таблицы",
          extensions: ["csv", "tsv", "xls", "xlsx", "xlsb", "xlsm", "ods"],
        },
      ],
    })) ?? [],
  chooseLegacy: () =>
    open({
      multiple: false,
      title: "Выберите базу Base Search 2.2.0",
      filters: [
        { name: "База SQLite", extensions: ["db", "sqlite", "sqlite3"] },
      ],
    }),
  chooseExport: (format) =>
    save({
      title: "Сохранить результаты",
      defaultPath: "base-search." + format,
      filters: [{ name: format.toUpperCase(), extensions: [format] }],
    }),
  preview: (path) => invoke("preview_file", { path }),
  import: (files) => invoke("start_import", { files }),
  migrate: (path) => invoke("start_migration", { path }),
  search: (query, offset, limit) => invoke("search", { query, offset, limit }),
  detail: (id, revision) => invoke("record_detail", { id, revision }),
  summary: (query, dimension) => invoke("summary", { query, dimension }),
  export: (query, path, columns) =>
    invoke("start_export", { query, path, columns }),
  job: (id) => invoke("job_status", { id }),
  cancel: (id) => invoke("cancel_job", { id }),
};
