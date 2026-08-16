/** What a badge needs to render. */
export interface BadgeProps {
    label: string;
    tone?: "muted" | "loud";
}

/** One badge. */
export const Badge = (props: BadgeProps) => (
    <span className={props.tone ?? "muted"}>{props.label}</span>
);

/** A list of badges, rendered in order. */
export class BadgeList {
    private items: BadgeProps[] = [];

    add(item: BadgeProps): void {
        this.items.push(item);
    }

    render(): JSX.Element {
        return <div>{this.items.map((item) => Badge(item))}</div>;
    }
}
