using System;
using System.Threading.Tasks;
using Windows.Media.Ocr;
using Windows.Graphics.Imaging;
using Windows.Storage;
using Windows.Globalization;

class Program
{
    static async Task Run(string imagePath, string language)
    {
        var file = await StorageFile.GetFileFromPathAsync(imagePath);
        var stream = await file.OpenAsync(FileAccessMode.Read);
        var decoder = await BitmapDecoder.CreateAsync(stream);
        var bitmap = await decoder.GetSoftwareBitmapAsync();

        OcrEngine engine;
        if (!string.IsNullOrEmpty(language))
        {
            var lang = new Language(language);
            engine = OcrEngine.TryCreateFromLanguage(lang);
            if (engine == null)
                engine = OcrEngine.TryCreateFromUserProfileLanguages();
        }
        else
        {
            engine = OcrEngine.TryCreateFromUserProfileLanguages();
        }
        if (engine == null)
        {
            Console.WriteLine("{\"error\":\"OCR engine unavailable\"}");
            return;
        }

        var result = await engine.RecognizeAsync(bitmap);
        var lines = new System.Collections.Generic.List<string>();
        foreach (var line in result.Lines)
            lines.Add(line.Text);

        var escaped = result.Text.Replace("\\", "\\\\").Replace("\"", "\\\"");
        var lineJsons = new System.Collections.Generic.List<string>();
        foreach (var l in lines)
            lineJsons.Add("\"" + l.Replace("\\", "\\\\").Replace("\"", "\\\"") + "\"");

        var wordCount = result.Text.Split(new char[] { ' ' }, StringSplitOptions.RemoveEmptyEntries).Length;
        Console.WriteLine(
            "{\"text\":\"" + escaped +
            "\",\"lines\":[" + string.Join(",", lineJsons.ToArray()) +
            "],\"language\":\"" + engine.RecognizerLanguage.LanguageTag +
            "\",\"word_count\":" + wordCount + "}");
        stream.Dispose();
    }

    static void Main(string[] args)
    {
        string imagePath = args.Length > 0 ? args[0] : "";
        string language = args.Length > 1 ? args[1] : "";
        try
        {
            Run(imagePath, language).GetAwaiter().GetResult();
        }
        catch (Exception e)
        {
            var msg = (e.InnerException?.Message ?? e.Message).Replace("\\", "\\\\").Replace("\"", "\\\"");
            Console.WriteLine("{\"error\":\"" + msg + "\"}");
        }
    }
}
