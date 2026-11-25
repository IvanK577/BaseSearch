import {
  Activity,
  ArrowLeft,
  ArrowRight,
  BarChart3,
  Bookmark,
  Building2,
  Check,
  ChevronDown,
  ChevronRight,
  Columns3,
  Database,
  Download,
  ExternalLink,
  FileUp,
  Filter,
  Flame,
  Languages,
  LayoutDashboard,
  LogOut,
  Menu,
  Moon,
  Plus,
  RefreshCw,
  Search,
  Settings,
  Sun,
  Trash2,
  TriangleAlert,
  Upload,
  UserRound,
  UsersRound,
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
  | "bookmark"
  | "trash"
  | "download"
  | "plus"
  | "alert"
  | "building"
  | "external"
  | "arrow-left"
  | "arrow-right"
  | "chevron-down"
  | "sun"
  | "moon"
  | "user"
  | "users"
  | "logout"
  | "refresh"
  | "language";

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
  bookmark: Bookmark,
  trash: Trash2,
  download: Download,
  plus: Plus,
  alert: TriangleAlert,
  building: Building2,
  external: ExternalLink,
  "arrow-left": ArrowLeft,
  "arrow-right": ArrowRight,
  "chevron-down": ChevronDown,
  sun: Sun,
  moon: Moon,
  user: UserRound,
  users: UsersRound,
  logout: LogOut,
  refresh: RefreshCw,
  language: Languages,
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
