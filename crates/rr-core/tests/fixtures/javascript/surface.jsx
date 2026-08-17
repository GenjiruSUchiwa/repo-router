export function Badge({ label }) {
  return <b className="x">{label}</b>;
}

export const BadgeList = ({ items }) => (
  <ul>
    {items.map((item) => (
      <Badge key={item} label={item} />
    ))}
  </ul>
);

export class Panel {
  render() {
    return <Badge label="ok" />;
  }
}
