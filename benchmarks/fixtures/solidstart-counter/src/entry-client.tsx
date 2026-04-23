import { StartClient, mount } from "@solidjs/start/client";

export default function entryClient() {
	mount(() => <StartClient />, document.getElementById("app")!);
}

entryClient();
