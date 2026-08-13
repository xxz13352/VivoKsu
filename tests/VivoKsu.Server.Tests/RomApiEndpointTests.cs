using System.Net;
using System.Net.Http.Json;
using FluentAssertions;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Mvc.Testing;
using Microsoft.Extensions.DependencyInjection;
using VivoKsu.Server.Models;
using VivoKsu.Server.Services;

namespace VivoKsu.Server.Tests;

public class RomApiEndpointTests : IClassFixture<RomApiFactory>
{
    private readonly RomApiFactory factory;

    public RomApiEndpointTests(RomApiFactory factory)
    {
        this.factory = factory;
    }

    [Fact]
    public async Task Rom_endpoint_returns_the_demo_rom_for_pd_and_version()
    {
        var client = factory.CreateClient();

        var response = await client.GetAsync("/api/rom?pd=PD2417&version=16.2.10.0");

        response.StatusCode.Should().Be(HttpStatusCode.OK);
        var rom = await response.Content.ReadFromJsonAsync<RomInfo>();
        rom.Should().NotBeNull();
        rom!.Pd.Should().Be("PD2417");
        rom.Version.Should().Be("16.2.10.0");
        rom.Url.Should().StartWith("https://");
    }

    [Fact]
    public async Task Rom_endpoint_requires_pd_and_version_parameters()
    {
        var client = factory.CreateClient();

        (await client.GetAsync("/api/rom")).StatusCode.Should().Be(HttpStatusCode.BadRequest);
        (await client.GetAsync("/api/rom?pd=PD2417")).StatusCode.Should().Be(HttpStatusCode.BadRequest);
        (await client.GetAsync("/api/rom?version=16.2.10.0")).StatusCode.Should().Be(HttpStatusCode.BadRequest);
    }

    [Fact]
    public async Task Health_endpoint_reports_ok()
    {
        var client = factory.CreateClient();

        var response = await client.GetAsync("/health");

        response.StatusCode.Should().Be(HttpStatusCode.OK);
    }
}

public sealed class RomApiFactory : WebApplicationFactory<Program>
{
    protected override void ConfigureWebHost(IWebHostBuilder builder)
    {
        builder.UseEnvironment("Testing");
        // 强制演示数据源:覆盖 appsettings.json 里可能填写的真实 VOTA 凭据。
        // Program.cs 的工厂 lambda 闭包捕获的是配置读取的值,必须从配置层清空。
        builder.UseSetting("VotaApi:User", " ");
        builder.UseSetting("VotaApi:Pass", " ");
        builder.UseSetting("VotaApi:ApiToken", " ");
        builder.UseSetting("VotaApi:DeviceId", " ");
    }
}
