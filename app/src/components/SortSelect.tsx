import { ArrowDownAZ, Clock, Flame, Heart, TrendingUp } from "lucide-react";
import { SearchSelect } from "@/components/SearchSelect";
import type { SortKey } from "@/lib/types";

interface SortSelectProps {
  value: SortKey;
  onChange: (key: SortKey) => void;
}

export function SortSelect({ value, onChange }: SortSelectProps) {
  return (
    <SearchSelect<SortKey>
      label="Sort by"
      value={value}
      onChange={onChange}
      widthClass="w-56"
      options={[
        { key: "download_total", label: "Most Downloaded", icon: TrendingUp },
        { key: "download_monthly", label: "Trending This Month", icon: Flame },
        { key: "last_updated", label: "Recently Updated", icon: Clock },
        { key: "favorite_total", label: "Most Favorited", icon: Heart },
        { key: "name", label: "Name (A–Z)", icon: ArrowDownAZ },
      ]}
    />
  );
}
