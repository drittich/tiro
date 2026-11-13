# Tiro

A transcription app similar to Anarlog that is designed for Fedora 44 KDE Plasma Desktop. It runs in the tray, detects when you are in an Ms Teams meeting (or possibly other online meeting like Zoom or Google Meet) and records the audio.

Ideally it can separate out each speaker.

It uses a speech-to-text LLM model from OpenRouter to periodically transcribe the audio in chunks so we get on the fly transcription without too much latency.

At the end, it will provide a summary of the transcript using Deepseek 4.1 Flash or similar model. You can download the transcript and/or summary as Markdown. You can also take notes in Markdown during or after the call.

