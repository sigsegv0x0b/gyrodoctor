package com.sigsegv0x0b.gyrodoctor

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.net.Uri
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.view.Gravity
import android.view.ViewGroup
import android.text.method.LinkMovementMethod
import android.text.util.Linkify
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.io.File
import java.io.FileOutputStream

class MainActivity : Activity() {
    private val inputUris = mutableListOf<Uri>()
    private var outputTreeUri: Uri? = null
    private lateinit var status: TextView
    private lateinit var inputsView: TextView
    private lateinit var logView: TextView
    private lateinit var runButton: Button

    private val pickInputs = 1001
    private val createOutput = 1002

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 2001)
        }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(32, 32, 32, 32)
        }
        val title = TextView(this).apply {
            text = "DJI Gyro Fixer"
            textSize = 26f
            gravity = Gravity.START
        }
        val subtitle = TextView(this).apply {
            text = "Merge DJI O4 chunks and repair their gyro data."
            textSize = 15f
            setPadding(0, 8, 0, 20)
        }
        val pickButton = Button(this).apply {
            text = "Select video chunks"
            setOnClickListener { pickVideos() }
        }
        val outputButton = Button(this).apply {
            text = "Choose output folder"
            setOnClickListener { chooseOutput() }
        }
        runButton = Button(this).apply {
            text = "Merge and repair"
            isEnabled = false
            setOnClickListener { runPipeline() }
        }
        val testButton = Button(this).apply {
            text = "Run files from app storage"
            setOnClickListener { runAppStorage() }
        }
        val fdButton = Button(this).apply {
            text = "Run app files via file descriptors"
            setOnClickListener { runAppStorageFds() }
        }
        val aboutButton = Button(this).apply {
            text = "About"
            setOnClickListener { showAbout() }
        }
        inputsView = TextView(this).apply {
            text = "No chunks selected."
            textSize = 13f
            setPadding(0, 12, 0, 12)
        }
        status = TextView(this).apply {
            text = "Ready."
            textSize = 14f
            setPadding(0, 12, 0, 12)
        }
        logView = TextView(this).apply {
            textSize = 11f
            setPadding(0, 8, 0, 8)
        }
        val scroll = ScrollView(this).apply {
            addView(logView)
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                0,
                1f,
            )
        }
        root.addView(title)
        root.addView(subtitle)
        root.addView(pickButton)
        root.addView(outputButton)
        root.addView(runButton)
        root.addView(testButton)
        root.addView(fdButton)
        root.addView(aboutButton)
        root.addView(inputsView)
        root.addView(status)
        root.addView(scroll)
        setContentView(root)
    }

    private fun pickVideos() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
            putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        }
        startActivityForResult(intent, pickInputs)
    }

    private fun chooseOutput() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
            addFlags(
                Intent.FLAG_GRANT_READ_URI_PERMISSION or
                    Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                    Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION,
            )
        }
        startActivityForResult(intent, createOutput)
    }

    private fun outputFolderLabel(uri: Uri): String {
        val id = DocumentsContract.getTreeDocumentId(uri)
        return id.substringAfterLast(':').ifEmpty { id }
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (resultCode != RESULT_OK || data == null) return
        if (requestCode == pickInputs) {
            inputUris.clear()
            val clip = data.clipData
            if (clip != null) {
                for (index in 0 until clip.itemCount) {
                    inputUris.add(clip.getItemAt(index).uri)
                }
            } else {
                data.data?.let { inputUris.add(it) }
            }
            for (uri in inputUris) {
                try {
                    contentResolver.takePersistableUriPermission(
                        uri,
                        Intent.FLAG_GRANT_READ_URI_PERMISSION,
                    )
                } catch (_: SecurityException) {
                }
            }
            updateUi()
        } else if (requestCode == createOutput) {
            outputTreeUri = data.data
            outputTreeUri?.let { uri ->
                try {
                    contentResolver.takePersistableUriPermission(
                        uri,
                        Intent.FLAG_GRANT_READ_URI_PERMISSION or
                            Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
                    )
                } catch (_: SecurityException) {
                }
            }
            updateUi()
        }
    }

    private fun updateUi() {
        val names = inputUris.map { displayName(it) }
        inputsView.text = if (names.isEmpty()) {
            "No chunks selected."
        } else {
            buildString {
                append("Chunks (${names.size}):\n")
                names.forEachIndexed { index, name -> append("${index + 1}. $name\n") }
                append("Output folder: ")
                append(outputTreeUri?.let { outputFolderLabel(it) } ?: "not selected")
                append("\nOutput file: joined-repaired.mp4")
            }
        }
        runButton.isEnabled = inputUris.size >= 2 && outputTreeUri != null
    }

    private fun displayName(uri: Uri): String {
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                return cursor.getString(0)
            }
        }
        return uri.lastPathSegment ?: uri.toString()
    }

    private fun appendLog(line: String) {
        logView.append(line + "\n")
    }

    private fun showAbout() {
        val message = TextView(this).apply {
            text = """
                DJI Gyro Fixer

                The gyro data repair algorithm is based on Gyro Doctor by DredSkyeQuads:
                https://github.com/DredSkyeQuads/ds-gyro-doctor

                Lossless MP4 chunk merging uses mp4-merge, the merge library used by
                Gyroflow:
                https://github.com/gyroflow/gyroflow

                Rust core and Android application: this project.
            """.trimIndent()
            textSize = 14f
            setPadding(48, 24, 48, 12)
            Linkify.addLinks(this, Linkify.WEB_URLS)
            movementMethod = LinkMovementMethod.getInstance()
        }
        AlertDialog.Builder(this)
            .setTitle("About")
            .setView(message)
            .setPositiveButton("Close", null)
            .show()
    }

    private fun startProcessing() {
        startService(Intent(this, ProcessingService::class.java))
    }

    private fun stopProcessing() {
        stopService(Intent(this, ProcessingService::class.java))
    }

    private fun runPipeline() {
        val tree = outputTreeUri ?: return
        if (inputUris.size < 2) return
        runButton.isEnabled = false
        status.text = "Creating output file..."
        val treeDocument = DocumentsContract.buildDocumentUriUsingTree(
            tree,
            DocumentsContract.getTreeDocumentId(tree),
        )
        val output = try {
            DocumentsContract.createDocument(
                contentResolver,
                treeDocument,
                "video/mp4",
                "joined-repaired.mp4",
            )
        } catch (error: Exception) {
            status.text = "Error creating output: ${error.message}"
            runButton.isEnabled = true
            return
        }
        if (output == null) {
            status.text = "The chosen folder did not create an output file."
            runButton.isEnabled = true
            return
        }
        status.text = "Opening files..."
        val pfds = ArrayList<ParcelFileDescriptor>()
        var outputPfd: ParcelFileDescriptor? = null
        val orderedUris = inputUris.sortedBy { displayName(it) }
        try {
            for (uri in orderedUris) {
                pfds.add(
                    contentResolver.openFileDescriptor(uri, "r")
                        ?: throw IllegalStateException("Cannot open $uri"),
                )
            }
            outputPfd = contentResolver.openFileDescriptor(output, "rw")
                ?: throw IllegalStateException("Cannot open output")
            FileOutputStream(outputPfd.fileDescriptor).use { it.channel.truncate(0) }
            val inputFds = pfds.map { it.fd }.toIntArray()
            val outFd = outputPfd.fd
            val out = outputPfd
            runButton.isEnabled = false
            startProcessing()
            Thread {
                val result = try {
                    Native.runMergeFds(
                        inputFds,
                        outFd,
                        "{\"force\":false}",
                        object : ProgressCallback {
                            override fun onProgress(stage: String, message: String, percent: Double) {
                                runOnUiThread {
                                    status.text = "$stage: $message"
                                    appendLog("$stage ${(percent * 100).toInt()}% $message")
                                }
                            }
                        },
                    )
                } catch (error: Throwable) {
                    "{\"ok\":false,\"error\":\"${error.message}\"}"
                }
                pfds.forEach { it.close() }
                out.close()
                if (!result.contains("\"ok\":true")) {
                    try {
                        DocumentsContract.deleteDocument(contentResolver, output)
                    } catch (_: Exception) {
                    }
                }
                stopProcessing()
                runOnUiThread {
                    status.text = result
                    appendLog(result)
                    runButton.isEnabled = inputUris.size >= 2 && outputTreeUri != null
                }
            }.start()
        } catch (error: Exception) {
            status.text = "Error: ${error.message}"
            appendLog(error.stackTraceToString())
            pfds.forEach { it.close() }
            outputPfd?.close()
            try {
                DocumentsContract.deleteDocument(contentResolver, output)
            } catch (_: Exception) {
            }
            stopProcessing()
            runButton.isEnabled = true
        }
    }

    private fun runAppStorage() {
        val base = filesDir
        val inputDir = File(base, "input")
        val outputDir = File(base, "output")
        outputDir.mkdirs()
        val inputs = inputDir.listFiles { file -> file.isFile && file.name.endsWith(".mp4", true) }
            ?.sortedBy { it.name }
            ?: emptyList()
        if (inputs.size < 2) {
            status.text = "Put at least two MP4 chunks in ${inputDir.absolutePath}"
            return
        }
        val output = File(outputDir, "joined-repaired.mp4")
        if (output.exists()) output.delete()
        runButton.isEnabled = false
        runNative(inputs.map { it.absolutePath }.toTypedArray(), output.absolutePath)
    }

    private fun runAppStorageFds() {
        val base = filesDir
        val inputDir = File(base, "input")
        val outputDir = File(base, "output")
        outputDir.mkdirs()
        val inputs = inputDir.listFiles { file -> file.isFile && file.name.endsWith(".mp4", true) }
            ?.sortedBy { it.name }
            ?: emptyList()
        if (inputs.size < 2) {
            status.text = "Put at least two MP4 chunks in ${inputDir.absolutePath}"
            return
        }
        val output = File(outputDir, "joined-repaired.mp4")
        val pfds = ArrayList<ParcelFileDescriptor>()
        val outputPfd = try {
            for (file in inputs) {
                pfds.add(ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY))
            }
            ParcelFileDescriptor.open(
                output,
                ParcelFileDescriptor.MODE_READ_WRITE or
                    ParcelFileDescriptor.MODE_CREATE or
                    ParcelFileDescriptor.MODE_TRUNCATE,
            )
        } catch (error: Exception) {
            status.text = "Error: ${error.message}"
            pfds.forEach { it.close() }
            return
        }
        runButton.isEnabled = false
        startProcessing()
        val inputFds = pfds.map { it.fd }.toIntArray()
        val outFd = outputPfd.fd
        Thread {
            val result = try {
                Native.runMergeFds(
                    inputFds,
                    outFd,
                    "{\"force\":false}",
                    object : ProgressCallback {
                        override fun onProgress(stage: String, message: String, percent: Double) {
                            runOnUiThread {
                                status.text = "$stage: $message"
                                appendLog("$stage ${(percent * 100).toInt()}% $message")
                            }
                        }
                    },
                )
            } catch (error: Throwable) {
                "{\"ok\":false,\"error\":\"${error.message}\"}"
            }
            pfds.forEach { it.close() }
            outputPfd.close()
            stopProcessing()
            runOnUiThread {
                status.text = result
                appendLog(result)
            }
        }.start()
    }

    private fun runNative(inputs: Array<String>, output: String) {
        status.text = "Starting..."
        startProcessing()
        Thread {
            val result = try {
                Native.runMerge(
                    inputs,
                    output,
                    "{\"force\":false}",
                    object : ProgressCallback {
                        override fun onProgress(stage: String, message: String, percent: Double) {
                            runOnUiThread {
                                status.text = "$stage: $message"
                                appendLog("$stage ${(percent * 100).toInt()}% $message")
                            }
                        }
                    },
                )
            } catch (error: Throwable) {
                "{\"ok\":false,\"error\":\"${error.message}\"}"
            }
            stopProcessing()
            runOnUiThread {
                status.text = result
                appendLog(result)
                runButton.isEnabled = inputUris.size >= 2 && outputTreeUri != null
            }
        }.start()
    }
}
