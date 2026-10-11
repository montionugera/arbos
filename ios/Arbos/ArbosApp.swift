import SwiftUI

@main
struct ArbosApp: App {
    var body: some Scene {
        WindowGroup {
            PhoneView()
        }
    }
}

struct PhoneView: View {
    @StateObject private var session = PhoneSession()
    @State private var url = ""
    @State private var accessId = ""
    @State private var accessSecret = ""
    @State private var prompt = ""

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Text("Arbos 0.2.111")
                    TextField("Engine URL", text: $url)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    SecureField("Access client id", text: $accessId)
                    SecureField("Access secret", text: $accessSecret)
                    Button("Connect") {
                        session.connect(
                            url: url,
                            accessId: accessId,
                            accessSecret: accessSecret
                        )
                    }
                    Text(session.status)
                }
                Section("Projects") {
                    if session.projects.isEmpty {
                        Text("No projects")
                    }
                    ForEach(session.projects) { project in
                        Button(project.name) {
                            session.follow(project)
                        }
                    }
                }
                if session.following != nil {
                    Section("Main") {
                        ForEach(Array(session.lines.enumerated()), id: \.offset) { _, line in
                            Text(line)
                        }
                        TextField("Prompt", text: $prompt)
                        Button("Send") {
                            session.send(prompt)
                            prompt = ""
                        }
                    }
                }
            }
            .navigationTitle("Arbos")
        }
    }
}
