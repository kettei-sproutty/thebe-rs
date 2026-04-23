import { resetCount } from "../state";

export async function POST() {
  return Response.json({ count: resetCount() });
}
