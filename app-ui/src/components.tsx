import { useEffect, useId, useRef, type ReactNode } from "react";
import { AlertCircle, X, Search, ListFilter, FolderInput } from "lucide-react";

export function Modal({
  title,
  children,
  close,
  wide,
  drawer,
}: {
  title: string;
  children: ReactNode;
  close: () => void;
  wide?: boolean;
  drawer?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const id = useId();
  useEffect(() => {
    const el = ref.current!;
    el.showModal();
    return () => el.close();
  }, []);
  return (
    <dialog
      ref={ref}
      aria-labelledby={id}
      className={drawer ? "drawer" : wide ? "modal wide" : "modal"}
      onCancel={(e) => {
        e.preventDefault();
        close();
      }}
    >
      <header className="modal-head">
        <h2 id={id}>{title}</h2>
        <button className="icon-button" aria-label="Закрыть" onClick={close}>
          <X size={18} />
        </button>
      </header>
      {children}
    </dialog>
  );
}

export function ErrorNote({
  children,
  retry,
}: {
  children: ReactNode;
  retry?: () => void;
}) {
  return (
    <div role="alert" className="error">
      <AlertCircle size={17} />
      <div>{children}</div>
      {retry && <button onClick={retry}>Повторить</button>}
    </div>
  );
}

export function Empty({
  kind,
  title,
  children,
}: {
  kind: "search" | "data" | "filter";
  title: string;
  children?: ReactNode;
}) {
  const Icon =
    kind === "data" ? FolderInput : kind === "filter" ? ListFilter : Search;
  return (
    <div className="empty">
      <Icon size={27} strokeWidth={1.4} />
      <h3>{title}</h3>
      {children}
    </div>
  );
}
