interface StatBarProps {
  label: string;
  percent: number;
  sublabel?: string;
}

export function StatBar({ label, percent, sublabel }: StatBarProps) {
  return (
    <div className="stat-bar">
      <div className="stat-bar-header">
        <span>{label}</span>
        <span>
          {percent.toFixed(0)}%{sublabel ? ` · ${sublabel}` : ""}
        </span>
      </div>
      <div className="stat-bar-track">
        <div
          className="stat-bar-fill"
          style={{ width: `${Math.min(percent, 100)}%` }}
        />
      </div>
    </div>
  );
}
