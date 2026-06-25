import { redirect } from "next/navigation";

export default function ExpiredPage() {
	redirect("/?expired=true");
}
