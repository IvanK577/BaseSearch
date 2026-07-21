import {
  Activity,
  BarChart3,
  Building2,
  Check,
  ChevronRight,
  Columns3,
  Database,
  Download,
  ExternalLink,
  FileUp,
  Filter,
  Flame,
  LayoutDashboard,
  Menu,
  Plus,
  Search,
  Settings,
  Trash2,
  TriangleAlert,
  Upload,
  X,
  type LucideIcon,
} from "lucide-react";

type IconName =
  | "home"
  | "search"
  | "analytics"
  | "import"
  | "export"
  | "columns"
  | "settings"
  | "jobs"
  | "close"
  | "chevron"
  | "flame"
  | "database"
  | "check"
  | "menu"
  | "filter"
  | "trash"
  | "download"
  | "plus"
  | "alert"
  | "building"
  | "external";

const ICONS: Record<IconName, LucideIcon> = {
  home: LayoutDashboard,
  search: Search,
  analytics: BarChart3,
  import: Upload,
  export: FileUp,
  columns: Columns3,
  settings: Settings,
  jobs: Activity,
  close: X,
  chevron: ChevronRight,
  flame: Flame,
  database: Database,
  check: Check,
  menu: Menu,
  filter: Filter,
  trash: Trash2,
  download: Download,
  plus: Plus,
  alert: TriangleAlert,
  building: Building2,
  external: ExternalLink,
};

export function Icon({
  name,
  size = 18,
  className,
}: {
  name: IconName;
  size?: number;
  className?: string;
}) {
  const Glyph = ICONS[name];
  return (
    <Glyph
      className={className}
      size={size}
      strokeWidth={1.75}
      aria-hidden="true"
      focusable="false"
    />
  );
}

export type { IconName };
