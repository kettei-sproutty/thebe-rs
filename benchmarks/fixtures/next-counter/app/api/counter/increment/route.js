import { incrementCount } from "../state";

export async function POST() {
  return Response.json({ count: incrementCount() });
}
